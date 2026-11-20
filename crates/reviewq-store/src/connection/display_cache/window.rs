//! Read one bounded window or schedule a cold index through a single build slot.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::Snapshot;
use reviewq_app::reports::{
    DISPLAY_INDEX_FORMAT, DisplayBlock, DisplayFallback, DisplayIndex, DisplayLayout,
    DisplayWindow, DisplayWindowBlock, DisplayWindowPosition, DisplayWindowState,
    MAX_DISPLAY_BLOCKS, MAX_DISPLAY_HEADINGS_BYTES, ReportReference, build_display_index,
};
use reviewq_domain::identity::{ReportHash, RunId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use uuid::Uuid;

use super::layout::{self, SPAN_COLUMNS, WINDOW_BYTES};
use super::{MAX_AGE_MS, open, publish};
use crate::connection::profile::Profile;
use crate::events::{now_ms, snapshot};
use crate::reports::{self, Version, authorize_run, read_artifact_bytes, read_markdown};
use crate::{SqliteStore, StoreError};

const MAX_WINDOW_READS: usize = 8;
const MAX_BLOCKS: usize = 32;
const BUILD_BACKOFF: Duration = Duration::from_secs(15);

#[derive(Clone, Eq, PartialEq)]
struct IndexKey {
    artifact: String,
    hash: ReportHash,
}

pub(in crate::connection) struct Builds {
    state: Mutex<BuildState>,
    reads: Arc<Semaphore>,
    released: watch::Sender<()>,
}

impl Default for Builds {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            reads: Arc::new(Semaphore::new(MAX_WINDOW_READS)),
            released: watch::channel(()).0,
        }
    }
}

#[derive(Default)]
struct BuildState {
    capture: Option<IndexKey>,
    cold: Option<IndexKey>,
    cooling_until: Option<Instant>,
}

#[derive(Clone, Copy)]
enum Lane {
    Capture,
    Cold,
}

struct Build {
    builds: Arc<Builds>,
    lane: Lane,
}

enum Claim {
    Started(Build),
    Pending,
    Busy,
    Cooling,
}

#[derive(Clone)]
struct CacheRead {
    profile: Arc<Profile>,
    slot: Arc<OwnedSemaphorePermit>,
}

enum CacheWindow {
    Missing,
    Building,
    Fallback(DisplayFallback),
    Ready(Spans),
}

struct Spans {
    blocks: Vec<DisplayBlock>,
    total_blocks: u32,
    previous: Option<u32>,
    next: Option<u32>,
    layout: Option<DisplayLayout>,
    byte_length: u32,
    scalar_length: u32,
    utf16_end: u32,
}

impl SqliteStore {
    pub(crate) async fn report_display_window(
        &self,
        actor: Actor,
        run: RunId,
        reference: ReportReference,
        position: DisplayWindowPosition,
    ) -> Result<Snapshot<DisplayWindow>, StoreError> {
        let slot = Arc::new(
            self.display_builds
                .reads
                .clone()
                .try_acquire_owned()
                .map_err(|_| AppError::Busy)?,
        );
        let (version, profile) = {
            let (actor, run, reference, slot) =
                (actor.clone(), run.clone(), reference.clone(), slot.clone());
            self.submit(false, move |db, profile| {
                let _slot = slot;
                let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
                let version = authorized_version(&tx, &actor, &run, &reference)?;
                tx.commit()?;
                Ok((version, profile.clone()))
            })
            .await?
        };
        validate_position(position, version.summary.stats.byte_length)?;
        let key = IndexKey {
            artifact: version.artifact_id,
            hash: reference.hash.clone(),
        };
        let store_id = self.identity().store_id;
        let cache_read = CacheRead {
            profile,
            slot: slot.clone(),
        };
        let window = if !version.summary.stats.complete {
            CacheWindow::Fallback(DisplayFallback::IncompleteSource)
        } else if version.summary.stats.require_markdown_budget().is_err() {
            CacheWindow::Fallback(DisplayFallback::DocumentBudget)
        } else {
            lookup(cache_read.clone(), store_id, key.clone(), position).await?
        };
        let window = match window {
            CacheWindow::Missing => {
                self.request_display_build(cache_read, key, &actor, &run, &reference, position)
                    .await?
            }
            found => found,
        };
        self.read(move |tx| {
            let _slot = slot;
            let version = authorized_version(tx, &actor, &run, &reference)?;
            let state = match window {
                CacheWindow::Building => DisplayWindowState::Building,
                CacheWindow::Ready(spans) => prepare_blocks(tx, &version, spans)?,
                CacheWindow::Fallback(reason) => DisplayWindowState::Fallback(reason),
                CacheWindow::Missing => return Err(AppError::Internal.into()),
            };
            snapshot(
                tx,
                DisplayWindow {
                    report: version.summary,
                    state,
                },
            )
        })
        .await
    }

    pub(super) async fn build_captured_index(
        &self,
        profile: Arc<Profile>,
        artifact: String,
        hash: ReportHash,
        source: Option<String>,
    ) -> Result<(), StoreError> {
        let key = IndexKey { artifact, hash };
        let mut released = self.display_builds.released.subscribe();
        let build = loop {
            released.borrow_and_update();
            match self.display_builds.claim(&key, Lane::Capture)? {
                Claim::Started(build) => break build,
                // Keep the source until a concurrent cold build has published
                // or released its slot after a canceled request.
                Claim::Pending => released.changed().await.map_err(|_| AppError::Internal)?,
                Claim::Busy | Claim::Cooling => return Err(AppError::Busy.into()),
            }
        };
        let store_id = self.identity().store_id;
        tokio::task::spawn_blocking(move || {
            let _build = build;
            build_and_publish(&profile, store_id, &key, source)
        })
        .await
        .map_err(|_| AppError::Internal)?
    }

    // The request owns the slot only while reading the admitted source. The owned
    // blocking task holds it through parse/publication even if the browser leaves.
    async fn request_display_build(
        &self,
        cache_read: CacheRead,
        key: IndexKey,
        actor: &Actor,
        run: &RunId,
        reference: &ReportReference,
        position: DisplayWindowPosition,
    ) -> Result<CacheWindow, StoreError> {
        let build = match self.display_builds.claim(&key, Lane::Cold)? {
            Claim::Started(build) => build,
            Claim::Pending => return Ok(CacheWindow::Building),
            Claim::Busy => return Err(AppError::Busy.into()),
            Claim::Cooling => return Ok(CacheWindow::Fallback(DisplayFallback::CacheUnavailable)),
        };

        let store_id = self.identity().store_id;
        // An earlier worker or normal capture may have published after our
        // first lookup and before this request reserved the build slot.
        match lookup(cache_read.clone(), store_id, key.clone(), position).await? {
            CacheWindow::Missing => {}
            found => return Ok(found),
        }

        let (actor, run, reference) = (actor.clone(), run.clone(), reference.clone());
        let runtime = Handle::current();
        self.submit(false, move |db, _| {
            // A canceled queued read is skipped by submit. If the SQLite read
            // has started, finish scheduling the build regardless of the caller.
            let CacheRead {
                profile,
                slot: _slot,
            } = cache_read;
            let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let version = authorized_version(&tx, &actor, &run, &reference)?;
            if !version.summary.stats.complete {
                return Err(AppError::PreconditionFailed.into());
            }
            let source = read_markdown(&tx, &version)?;
            tx.commit()?;
            runtime.spawn_blocking(move || {
                if build_and_publish(&profile, store_id, &key, Some(source)).is_err() {
                    build.cool_down();
                }
            });
            Ok(())
        })
        .await?;
        Ok(CacheWindow::Building)
    }
}

impl Builds {
    fn claim(self: &Arc<Self>, key: &IndexKey, lane: Lane) -> Result<Claim, StoreError> {
        let mut state = self.state.lock().map_err(|_| AppError::Internal)?;
        if state.capture.as_ref() == Some(key) || state.cold.as_ref() == Some(key) {
            return Ok(Claim::Pending);
        }
        if matches!(lane, Lane::Cold)
            && state
                .cooling_until
                .is_some_and(|until| until > Instant::now())
        {
            return Ok(Claim::Cooling);
        }
        let active = state.active(lane);
        if active.is_some() {
            return Ok(Claim::Busy);
        }
        *active = Some(key.clone());
        Ok(Claim::Started(Build {
            builds: self.clone(),
            lane,
        }))
    }
}

impl BuildState {
    fn active(&mut self, lane: Lane) -> &mut Option<IndexKey> {
        match lane {
            Lane::Capture => &mut self.capture,
            Lane::Cold => &mut self.cold,
        }
    }
}

impl Build {
    fn cool_down(&self) {
        if let Ok(mut state) = self.builds.state.lock() {
            state.cooling_until = Some(Instant::now() + BUILD_BACKOFF);
        }
    }
}

impl Drop for Build {
    fn drop(&mut self) {
        if let Ok(mut state) = self.builds.state.lock() {
            *state.active(self.lane) = None;
        }
        self.builds.released.send_replace(());
    }
}

fn build_and_publish(
    profile: &Profile,
    store_id: Uuid,
    key: &IndexKey,
    source: Option<String>,
) -> Result<(), StoreError> {
    let mut cache = open(profile, store_id, true)?.ok_or(AppError::Internal)?;
    if cache_entry(&cache, key)?.is_some() {
        return Ok(());
    }
    let index = if let Some(source) = source {
        if ReportHash::from_sha256(Sha256::digest(source.as_bytes()).into()) != key.hash {
            return Err(AppError::PreconditionFailed.into());
        }
        std::panic::catch_unwind(|| build_display_index(&source))
            .unwrap_or(DisplayIndex::Fallback(DisplayFallback::ParserFailed))
    } else {
        DisplayIndex::Fallback(DisplayFallback::DocumentBudget)
    };
    publish(
        &mut cache,
        &key.artifact,
        key.hash.as_str(),
        DISPLAY_INDEX_FORMAT,
        index,
    )
}

fn authorized_version(
    connection: &Connection,
    actor: &Actor,
    run: &RunId,
    reference: &ReportReference,
) -> Result<Version, StoreError> {
    authorize_run(connection, actor, run)?;
    let version = reports::version(connection, run, &reference.receipt_id)?;
    if version.summary.reference.hash != reference.hash {
        return Err(AppError::PreconditionFailed.into());
    }
    Ok(version)
}

fn validate_position(position: DisplayWindowPosition, byte_length: u64) -> Result<(), StoreError> {
    match position {
        DisplayWindowPosition::AtByte { offset, .. } if offset > byte_length => {
            Err(AppError::RangeNotSatisfiable.into())
        }
        DisplayWindowPosition::Exact { from, to }
            if from >= to || to > byte_length || to - from > u64::from(WINDOW_BYTES) =>
        {
            Err(AppError::RangeNotSatisfiable.into())
        }
        DisplayWindowPosition::Before(byte) | DisplayWindowPosition::After(byte)
            if u64::from(byte) >= byte_length =>
        {
            Err(AppError::CursorExpired.into())
        }
        _ => Ok(()),
    }
}

async fn lookup(
    cache_read: CacheRead,
    store_id: Uuid,
    key: IndexKey,
    position: DisplayWindowPosition,
) -> Result<CacheWindow, StoreError> {
    tokio::task::spawn_blocking(move || {
        let CacheRead {
            profile,
            slot: _slot,
        } = cache_read;
        let result = (|| {
            let Some(mut cache) = open(&profile, store_id, false)? else {
                return Ok(CacheWindow::Missing);
            };
            let tx = cache.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let window = cache_window(&tx, &key, position)?;
            tx.commit()?;
            Ok::<_, StoreError>(window)
        })();
        // The cache is optional, including corrupt and inaccessible sidecars.
        match result {
            Ok(window) => Ok(window),
            Err(error) => match error.app() {
                error @ (AppError::RangeNotSatisfiable | AppError::CursorExpired) => Err(error),
                _ => Ok(CacheWindow::Fallback(DisplayFallback::CacheUnavailable)),
            },
        }
    })
    .await
    .map_err(|_| AppError::Internal)?
    .map_err(StoreError::from)
}

struct CacheEntry {
    id: i64,
    span_count: u32,
    fallback: Option<String>,
    byte_length: u32,
    scalar_length: u32,
    utf16_length: u32,
}

fn cache_entry(connection: &Connection, key: &IndexKey) -> Result<Option<CacheEntry>, StoreError> {
    let found = connection
        .query_row(
            "SELECT id, span_count, fallback, byte_length, scalar_length, utf16_length, checksum
             FROM display_entries
             WHERE artifact_id=?1 AND sha256=?2 AND index_format=?3 AND published_at_ms>=?4",
            params![
                key.artifact,
                key.hash.as_str(),
                DISPLAY_INDEX_FORMAT,
                now_ms()?.saturating_sub(MAX_AGE_MS)
            ],
            |row| {
                Ok((
                    CacheEntry {
                        id: row.get(0)?,
                        span_count: row.get(1)?,
                        fallback: row.get(2)?,
                        byte_length: row.get(3)?,
                        scalar_length: row.get(4)?,
                        utf16_length: row.get(5)?,
                    },
                    row.get::<_, Vec<u8>>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((entry, checksum)) = found else {
        return Ok(None);
    };
    let lengths = [
        entry.span_count,
        entry.byte_length,
        entry.scalar_length,
        entry.utf16_length,
    ];
    if layout::header_checksum(key.hash.as_str(), lengths, entry.fallback.as_deref()).as_slice()
        != checksum.as_slice()
    {
        return Err(AppError::Internal.into());
    }
    Ok(Some(entry))
}

fn cache_window(
    connection: &Connection,
    key: &IndexKey,
    position: DisplayWindowPosition,
) -> Result<CacheWindow, StoreError> {
    let Some(entry) = cache_entry(connection, key)? else {
        return Ok(CacheWindow::Missing);
    };
    if let Some(reason) = &entry.fallback {
        return Ok(CacheWindow::Fallback(parse_fallback(reason)?));
    }
    if entry.span_count == 0 || entry.span_count as usize > MAX_DISPLAY_BLOCKS {
        return Err(AppError::Internal.into());
    }

    let id = entry.id;
    let mut end_bound = u64::from(entry.byte_length);
    let (boundary, comparison, order) = match position {
        DisplayWindowPosition::AtByte { offset, .. } => {
            let start: u32 = connection.query_row(
                "SELECT coalesce(
                   (SELECT byte_start FROM display_spans WHERE entry_id=?1 AND byte_start<=?2 ORDER BY byte_start DESC LIMIT 1),
                   (SELECT byte_start FROM display_spans WHERE entry_id=?1 ORDER BY byte_start ASC LIMIT 1)
                 )",
                params![id, offset as i64],
                |row| row.get(0),
            )?;
            (u64::from(start), ">=", "ASC")
        }
        DisplayWindowPosition::Before(offset) => (u64::from(offset), "<", "DESC"),
        DisplayWindowPosition::After(offset) => (u64::from(offset), ">", "ASC"),
        DisplayWindowPosition::Exact { from, to } => {
            end_bound = to;
            (from, ">=", "ASC")
        }
    };
    let sql = format!(
        "SELECT {SPAN_COLUMNS} FROM display_spans
         WHERE entry_id=?1 AND byte_start {comparison} ?2 AND byte_start<?3
         ORDER BY byte_start {order} LIMIT {MAX_BLOCKS}"
    );
    let report_hash = key.hash.as_str();
    let mut statement = connection.prepare_cached(&sql)?;
    let mut rows = statement.query(params![id, boundary as i64, end_bound as i64])?;
    let mut blocks = Vec::new();
    let mut byte_count = 0;
    while let Some(row) = rows.next()? {
        let span = layout::read_span(row, report_hash)?;
        byte_count += span.byte_end - span.byte_start;
        if byte_count > WINDOW_BYTES {
            break;
        }
        blocks.push(span);
    }
    if order == "DESC" {
        blocks.reverse();
    }
    if let DisplayWindowPosition::Exact { from, to } = position
        && (blocks
            .first()
            .is_none_or(|span| u64::from(span.byte_start) != from)
            || blocks
                .last()
                .is_none_or(|span| u64::from(span.byte_end) != to))
    {
        return Err(AppError::RangeNotSatisfiable.into());
    }
    let start = blocks.first().ok_or(AppError::Internal)?.byte_start;
    let last = blocks.last().ok_or(AppError::Internal)?;
    let has_previous: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM display_spans WHERE entry_id=?1 AND byte_start<?2)",
        params![id, start],
        |row| row.get(0),
    )?;
    let (has_next, utf16_end) = next_span(connection, &entry, report_hash, last)?;
    let layout = if matches!(
        position,
        DisplayWindowPosition::AtByte {
            with_layout: true,
            ..
        }
    ) {
        Some(read_layout(connection, &entry, report_hash)?)
    } else {
        None
    };
    let next = has_next.then_some(last.byte_start);
    Ok(CacheWindow::Ready(Spans {
        blocks,
        total_blocks: entry.span_count,
        previous: has_previous.then_some(start),
        next,
        byte_length: entry.byte_length,
        scalar_length: entry.scalar_length,
        utf16_end,
        layout,
    }))
}

fn next_span(
    connection: &Connection,
    entry: &CacheEntry,
    report_hash: &str,
    last: &DisplayBlock,
) -> Result<(bool, u32), StoreError> {
    let sql = format!(
        "SELECT {SPAN_COLUMNS} FROM display_spans WHERE entry_id=?1 AND byte_start>?2
         ORDER BY byte_start ASC LIMIT 1"
    );
    let mut statement = connection.prepare_cached(&sql)?;
    let mut rows = statement.query(params![entry.id, last.byte_start])?;
    if let Some(row) = rows.next()? {
        let next = layout::read_span(row, report_hash)?;
        if last.byte_end == next.byte_start {
            return Ok((true, next.utf16_start));
        }
    } else if last.byte_end == entry.byte_length {
        return Ok((false, entry.utf16_length));
    }
    Err(AppError::Internal.into())
}

fn read_layout(
    connection: &Connection,
    entry: &CacheEntry,
    report_hash: &str,
) -> Result<DisplayLayout, StoreError> {
    let limit = MAX_DISPLAY_BLOCKS + 1;
    let sql = format!(
        "SELECT {SPAN_COLUMNS} FROM display_spans WHERE entry_id=?1 ORDER BY byte_start ASC LIMIT {limit}"
    );
    let mut statement = connection.prepare_cached(&sql)?;
    let mut rows = statement.query([entry.id])?;
    let mut blocks: Vec<DisplayBlock> = Vec::with_capacity(entry.span_count as usize);
    let mut heading_bytes = 0;
    while let Some(row) = rows.next()? {
        let span = layout::read_span(row, report_hash)?;
        let agrees = blocks.last().map_or(
            span.scalar_start == span.byte_start && span.utf16_start == span.byte_start,
            |previous| {
                previous.byte_end == span.byte_start
                    && valid_length(
                        previous,
                        span.byte_start,
                        span.scalar_start,
                        span.utf16_start,
                    )
            },
        );
        heading_bytes += span.heading_markdown.as_ref().map_or(0, String::len);
        if !agrees
            || heading_bytes > MAX_DISPLAY_HEADINGS_BYTES
            || blocks.len() == entry.span_count as usize
        {
            return Err(AppError::Internal.into());
        }
        blocks.push(span);
    }
    let last = blocks.last().ok_or(AppError::Internal)?;
    if blocks.len() != entry.span_count as usize
        || last.byte_end != entry.byte_length
        || !valid_length(
            last,
            entry.byte_length,
            entry.scalar_length,
            entry.utf16_length,
        )
    {
        return Err(AppError::Internal.into());
    }
    Ok(DisplayLayout {
        byte_length: entry.byte_length,
        scalar_length: entry.scalar_length,
        utf16_length: entry.utf16_length,
        blocks,
    })
}

fn valid_length(span: &DisplayBlock, bytes: u32, scalars: u32, utf16: u32) -> bool {
    scalars > span.scalar_start
        && utf16 > span.utf16_start
        && utf16 - span.utf16_start >= scalars - span.scalar_start
        && utf16 - span.utf16_start <= bytes - span.byte_start
}

fn prepare_blocks(
    connection: &Connection,
    version: &Version,
    spans: Spans,
) -> Result<DisplayWindowState, StoreError> {
    let stats = &version.summary.stats;
    stats.require_markdown_budget()?;
    let start = spans.blocks.first().ok_or(AppError::Internal)?.byte_start;
    let end = spans.blocks.last().ok_or(AppError::Internal)?.byte_end;
    let invalid = || {
        Ok(DisplayWindowState::Fallback(
            DisplayFallback::InvalidBoundary,
        ))
    };
    if u64::from(spans.byte_length) != stats.byte_length
        || u64::from(spans.scalar_length) != stats.total_code_points
        || u64::from(end) > stats.byte_length
        || spans.total_blocks < spans.blocks.len() as u32
        || u64::from(spans.total_blocks) > stats.line_count
        || spans
            .blocks
            .windows(2)
            .any(|pair| pair[0].byte_end != pair[1].byte_start)
    {
        return invalid();
    }
    let source = read_artifact_bytes(
        connection,
        &version.artifact_id,
        u64::from(start),
        u64::from(end),
    )?;
    let Some(mut scalar) = source.first_scalar else {
        return invalid();
    };
    let mut utf16 = spans.blocks[0].utf16_start;
    let mut blocks = Vec::with_capacity(spans.blocks.len());
    for span in spans.blocks {
        let range = (span.byte_start - start) as usize..(span.byte_end - start) as usize;
        let Some(markdown) = source
            .bytes
            .get(range)
            .and_then(|slice| str::from_utf8(slice).ok())
        else {
            return invalid();
        };
        if u64::from(span.scalar_start) != scalar
            || span.utf16_start != utf16
            || span
                .heading_markdown
                .as_ref()
                .is_some_and(|heading| !markdown.starts_with(heading))
        {
            return invalid();
        }
        for ch in markdown.chars() {
            scalar += 1;
            utf16 += ch.len_utf16() as u32;
        }
        blocks.push(DisplayWindowBlock {
            span,
            markdown: markdown.to_owned(),
        });
    }
    if utf16 != spans.utf16_end
        || scalar > stats.total_code_points
        || (u64::from(end) == stats.byte_length && scalar != stats.total_code_points)
    {
        return invalid();
    }
    Ok(DisplayWindowState::Ready {
        total_blocks: spans.total_blocks,
        blocks,
        previous: spans.previous,
        next: spans.next,
        layout: spans.layout,
    })
}

fn parse_fallback(reason: &str) -> Result<DisplayFallback, StoreError> {
    Ok(match reason {
        "document_budget" => DisplayFallback::DocumentBudget,
        "parser_budget" => DisplayFallback::ParserBudget,
        "document_context" => DisplayFallback::DocumentContext,
        "layout_budget" => DisplayFallback::LayoutBudget,
        "unsupported_source" => DisplayFallback::UnsupportedSource,
        "invalid_boundary" => DisplayFallback::InvalidBoundary,
        "parser_failed" => DisplayFallback::ParserFailed,
        "incomplete_source" => DisplayFallback::IncompleteSource,
        "cache_unavailable" => DisplayFallback::CacheUnavailable,
        _ => return Err(AppError::Internal.into()),
    })
}
