//! Read one bounded window or schedule a cold index through a single build slot.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::Snapshot;
use reviewq_app::reports::{
    DISPLAY_INDEX_FORMAT, DisplayBlock, DisplayBlockKind, DisplayFallback, DisplayIndex,
    DisplayWindow, DisplayWindowBlock, DisplayWindowPosition, DisplayWindowState, ReportReference,
    build_display_index,
};
use reviewq_domain::identity::{ReportHash, RunId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use uuid::Uuid;

use super::{MAX_AGE_MS, open, publish};
use crate::connection::profile::Profile;
use crate::events::{now_ms, snapshot};
use crate::reports::{self, Version, authorize_run, read_artifact_bytes, read_markdown};
use crate::{SqliteStore, StoreError};

const MAX_WINDOW_READS: usize = 8;
const MAX_BLOCKS: usize = 32;
const WINDOW_BYTES: u32 = 64 * 1024;
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
        DisplayWindowPosition::AtByte(offset) if offset > byte_length => {
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
        result.unwrap_or(CacheWindow::Fallback(DisplayFallback::CacheUnavailable))
    })
    .await
    .map_err(|_| AppError::Internal.into())
}

type CacheEntry = (i64, u32, Option<String>);

fn cache_entry(connection: &Connection, key: &IndexKey) -> Result<Option<CacheEntry>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT id, span_count, fallback FROM display_entries
         WHERE artifact_id=?1 AND sha256=?2 AND index_format=?3 AND published_at_ms>=?4",
            params![
                key.artifact,
                key.hash.as_str(),
                DISPLAY_INDEX_FORMAT,
                now_ms()?.saturating_sub(MAX_AGE_MS)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
}

fn cache_window(
    connection: &Connection,
    key: &IndexKey,
    position: DisplayWindowPosition,
) -> Result<CacheWindow, StoreError> {
    let entry = cache_entry(connection, key)?;
    let Some((id, total_blocks, fallback)) = entry else {
        return Ok(CacheWindow::Missing);
    };
    if let Some(reason) = fallback {
        return Ok(CacheWindow::Fallback(parse_fallback(&reason)?));
    }

    let (boundary, sql, reverse) = match position {
        DisplayWindowPosition::AtByte(offset) => {
            let start: u32 = connection.query_row(
                "SELECT coalesce(
                   (SELECT byte_start FROM display_spans WHERE entry_id=?1 AND byte_start<=?2 ORDER BY byte_start DESC LIMIT 1),
                   (SELECT byte_start FROM display_spans WHERE entry_id=?1 ORDER BY byte_start LIMIT 1)
                 )",
                params![id, offset as i64],
                |row| row.get(0),
            )?;
            (start, "byte_start>=?2 ORDER BY byte_start ASC", false)
        }
        DisplayWindowPosition::After(offset) => {
            (offset, "byte_start>?2 ORDER BY byte_start ASC", false)
        }
        DisplayWindowPosition::Before(offset) => {
            (offset, "byte_start<?2 ORDER BY byte_start DESC", true)
        }
    };
    let sql = format!(
        "SELECT byte_start, byte_end, scalar_start, kind, heading_level FROM display_spans
         WHERE entry_id=?1 AND {sql} LIMIT {MAX_BLOCKS}"
    );
    let mut query = connection.prepare_cached(&sql)?;
    let mut rows = query.query(params![id, boundary])?;
    let mut blocks = Vec::new();
    let mut byte_count = 0_u32;
    while let Some(row) = rows.next()? {
        let start: u32 = row.get(0)?;
        let end: u32 = row.get(1)?;
        let length = end
            .checked_sub(start)
            .filter(|size| *size != 0)
            .ok_or(AppError::Internal)?;
        let projected = byte_count.checked_add(length).ok_or(AppError::Internal)?;
        if !blocks.is_empty() && projected > WINDOW_BYTES {
            break;
        }
        blocks.push(DisplayBlock {
            byte_start: start,
            byte_end: end,
            scalar_start: row.get(2)?,
            kind: parse_kind(
                row.get_ref(3)?.as_str().map_err(rusqlite::Error::from)?,
                row.get(4)?,
            )?,
        });
        byte_count = projected;
    }
    if reverse {
        blocks.reverse();
    }
    let start = blocks.first().ok_or(AppError::Internal)?.byte_start;
    let end = blocks.last().ok_or(AppError::Internal)?.byte_start;
    let (has_previous, has_next): (bool, bool) = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM display_spans WHERE entry_id=?1 AND byte_start<?2),
                EXISTS(SELECT 1 FROM display_spans WHERE entry_id=?1 AND byte_start>?3)",
        params![id, start, end],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(CacheWindow::Ready(Spans {
        blocks,
        total_blocks,
        previous: has_previous.then_some(start),
        next: has_next.then_some(end),
    }))
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
    if u64::from(end) > stats.byte_length
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
        if u64::from(span.scalar_start) != scalar {
            return invalid();
        }
        scalar += markdown.chars().count() as u64;
        blocks.push(DisplayWindowBlock {
            span,
            markdown: markdown.to_owned(),
        });
    }
    if scalar > stats.total_code_points
        || (u64::from(end) == stats.byte_length && scalar != stats.total_code_points)
    {
        return invalid();
    }
    Ok(DisplayWindowState::Ready {
        total_blocks: spans.total_blocks,
        blocks,
        previous: spans.previous,
        next: spans.next,
    })
}

fn parse_kind(kind: &str, level: Option<u8>) -> Result<DisplayBlockKind, StoreError> {
    Ok(match (kind, level) {
        ("paragraph", None) => DisplayBlockKind::Paragraph,
        ("heading", Some(level @ 1..=6)) => DisplayBlockKind::Heading(level),
        ("block_quote", None) => DisplayBlockKind::BlockQuote,
        ("code", None) => DisplayBlockKind::Code,
        ("list", None) => DisplayBlockKind::List,
        ("rule", None) => DisplayBlockKind::Rule,
        ("table", None) => DisplayBlockKind::Table,
        _ => return Err(AppError::Internal.into()),
    })
}

fn parse_fallback(reason: &str) -> Result<DisplayFallback, StoreError> {
    Ok(match reason {
        "document_budget" => DisplayFallback::DocumentBudget,
        "parser_budget" => DisplayFallback::ParserBudget,
        "document_context" => DisplayFallback::DocumentContext,
        "unsupported_source" => DisplayFallback::UnsupportedSource,
        "invalid_boundary" => DisplayFallback::InvalidBoundary,
        "parser_failed" => DisplayFallback::ParserFailed,
        "incomplete_source" => DisplayFallback::IncompleteSource,
        "cache_unavailable" => DisplayFallback::CacheUnavailable,
        _ => return Err(AppError::Internal.into()),
    })
}
