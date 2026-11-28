//! Short-lived layouts and sparse decoded-to-native offsets. The native file stays authoritative.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::history::document::{Index, MAX_INDEX_BYTES, Query, SourcePages, Window};
use reviewq_app::history::{FileContentRef, search::SourceObservation};
use reviewq_app::wait::{self, Place, Places, Turn};
use reviewq_domain::identity::ReportHash;
use reviewq_domain::session::SessionTarget;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, OwnedSemaphorePermit};
use tokio::time::Instant as Deadline;

use super::{file, locator::Source, native_string::NativeString, records, search};

const MAX_DOCUMENTS: usize = 3;
const RETENTION: Duration = Duration::from_secs(30);

type Key = (SessionTarget, FileContentRef);
type Handle = Arc<AsyncMutex<Option<Prepared>>>;

pub(super) struct Documents {
    active: Arc<Mutex<HashMap<Key, Slot>>>,
    /// One per document in use, shared by every caller of that document, so a caller that
    /// holds one always finds a free place or a document no one else is using.
    uses: Places,
    collect: AtomicBool,
}

impl Default for Documents {
    fn default() -> Self {
        Self {
            active: Arc::default(),
            uses: Places::new(MAX_DOCUMENTS),
            collect: AtomicBool::default(),
        }
    }
}

/// A document's handle, with the document's place among `MAX_DOCUMENTS`. The handle is
/// declared first, so it is let go of before the place.
pub(super) struct Claim {
    pub(super) handle: Handle,
    _place: Place,
}

struct Slot {
    touched: Instant,
    handle: Handle,
    /// Held by every claim of the document; it is in use while its place is held.
    place: Weak<OwnedSemaphorePermit>,
}

impl Documents {
    /// The handle of a document. A caller of a document in use joins its place; a caller
    /// of any other document waits until `deadline` for a place when every document is in
    /// use, and joins the document if another caller claims it meanwhile.
    pub(super) async fn claim(
        &self,
        target: SessionTarget,
        reference: FileContentRef,
        deadline: Deadline,
    ) -> Result<Claim, AppError> {
        let key = (target, reference);
        let mut turn = self.uses.turn(deadline);
        loop {
            if let Some(claim) = self.slot(&key, &mut turn)? {
                return Ok(claim);
            }
            turn.wait().await?;
        }
    }

    /// The handle of a document like `claim`, for a search on a blocking thread: it waits its
    /// turn the same way and leaves once `stop` is set.
    pub(super) fn claim_blocking(
        &self,
        target: SessionTarget,
        reference: FileContentRef,
        deadline: Deadline,
        stop: &AtomicBool,
    ) -> Result<Claim, AppError> {
        let key = (target, reference);
        let mut turn = self.uses.turn(deadline);
        loop {
            if let Some(claim) = self.slot(&key, &mut turn)? {
                return Ok(claim);
            }
            turn.wait_blocking(stop)?;
        }
    }

    /// The handle of a document in use or with a place free now; `Busy` otherwise. The
    /// background search index skips a document then, leaving the places to callers.
    pub(super) fn try_claim(
        &self,
        target: SessionTarget,
        reference: FileContentRef,
    ) -> Result<Claim, AppError> {
        self.slot(&(target, reference), &mut self.uses.turn(wait::deadline()))?
            .ok_or(AppError::Busy)
    }

    /// The document's claim, or `None` when it needs a place `turn` does not have.
    fn slot(&self, key: &Key, turn: &mut Turn<'_>) -> Result<Option<Claim>, AppError> {
        if !self.collect.swap(true, Ordering::Relaxed) {
            let owner = Arc::downgrade(&self.active);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(RETENTION / 3).await;
                    let Some(active) = owner.upgrade() else {
                        break;
                    };
                    if let Ok(mut active) = active.try_lock() {
                        active.retain(|_, slot| alive(slot));
                    }
                }
            });
        }
        let mut active = self.active.lock().map_err(|_| AppError::Internal)?;
        active.retain(|_, slot| alive(slot));
        if let Some(slot) = active.get_mut(key) {
            let Some(place) = slot.place.upgrade().or_else(|| {
                let place = turn.take()?;
                slot.place = Arc::downgrade(&place);
                Some(place)
            }) else {
                return Ok(None);
            };
            slot.touched = Instant::now();
            return Ok(Some(Claim {
                handle: slot.handle.clone(),
                _place: place,
            }));
        }
        let Some(place) = turn.take() else {
            return Ok(None);
        };
        if active.len() >= MAX_DOCUMENTS {
            // The caller's place leaves at most `MAX_DOCUMENTS - 1` documents in use by others.
            let oldest = active
                .iter()
                .filter(|(_, slot)| slot.place.strong_count() == 0)
                .min_by_key(|(_, slot)| slot.touched)
                .map(|(key, _)| key.clone())
                .ok_or(AppError::Internal)?;
            active.remove(&oldest);
        }
        let handle = Handle::default();
        active.insert(
            key.clone(),
            Slot {
                touched: Instant::now(),
                handle: handle.clone(),
                place: Arc::downgrade(&place),
            },
        );
        Ok(Some(Claim {
            handle,
            _place: place,
        }))
    }
}

fn alive(slot: &Slot) -> bool {
    slot.touched.elapsed() < RETENTION || slot.place.strong_count() > 0
}

pub(super) struct Prepared {
    observed: SourceObservation,
    fields: Vec<Field>,
    index: Index,
}

struct Field {
    start: u64,
    end: u64,
    string: NativeString,
}

pub(super) fn read(
    slot: &mut Option<Prepared>,
    source: &Source,
    reference: &FileContentRef,
    query: Query,
    cancel: &AtomicBool,
) -> Result<Window, AppError> {
    file::verify_content(source, reference)?;
    let metadata = source.file.metadata().map_err(|_| AppError::Unavailable)?;
    let observed = search::observed(source, &metadata, reference.position.high_water);
    if slot
        .as_ref()
        .is_none_or(|prepared| prepared.observed != observed)
    {
        *slot = Some(prepare(source, reference, observed.clone(), cancel)?);
    }
    let prepared = slot.as_ref().ok_or(AppError::Internal)?;
    let result = prepared.index.read(query, |bytes| {
        let mut output = Vec::with_capacity((bytes.end - bytes.start) as usize);
        for field in prepared.fields[prepared
            .fields
            .partition_point(|field| field.end <= bytes.start)..]
            .iter()
            .take_while(|field| field.start < bytes.end)
        {
            let from = bytes.start.max(field.start) - field.start;
            let to = bytes.end.min(field.end) - field.start;
            field
                .string
                .read(&source.file, from, to, cancel, &mut |offset, text| {
                    let low = from.saturating_sub(offset) as usize;
                    let high = (to - offset).min(text.len() as u64) as usize;
                    output.extend_from_slice(
                        text.as_bytes()
                            .get(low..high)
                            .ok_or(AppError::CursorExpired)?,
                    );
                    Ok(true)
                })?;
        }
        if output.len() as u64 != bytes.end - bytes.start {
            return Err(AppError::CursorExpired);
        }
        Ok(output)
    })?;
    search::verify_observation(source, &prepared.observed)?;
    Ok(result)
}

fn prepare(
    source: &Source,
    reference: &FileContentRef,
    observed: SourceObservation,
    cancel: &AtomicBool,
) -> Result<Prepared, AppError> {
    let base = reference.position.offset;
    let (end, bytes, values) = file::document_source(source, reference, cancel)?;
    let mut decoded = None;
    if bytes <= MAX_INDEX_BYTES as u64 {
        decoded = Some(Vec::with_capacity(bytes as usize));
    }
    let mut hash = Sha256::new();
    let mut pages = SourcePages::for_bytes(bytes);
    let mut fields = Vec::with_capacity(values.len());
    let mut start = 0;
    for (length, span) in values {
        let range = native_span(base, end, span)?;
        let string = NativeString::prepare_with(&source.file, range, 0, length, cancel, |text| {
            hash.update(text.as_bytes());
            pages.push(text);
            if let Some(decoded) = decoded.as_mut() {
                decoded.extend_from_slice(text.as_bytes());
            }
        })?;
        let end = start + length;
        fields.push(Field { start, end, string });
        start = end;
    }
    if start != bytes {
        return Err(AppError::CursorExpired);
    }
    search::verify_observation(source, &observed)?;
    let hash = ReportHash::from_sha256(hash.finalize().into());
    let text = decoded
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_| AppError::CursorExpired)?;
    let index = Index::new(hash, pages, text);
    if cancel.load(Ordering::Relaxed) {
        return Err(AppError::Unavailable);
    }
    Ok(Prepared {
        observed,
        fields,
        index,
    })
}

fn native_span(
    base: u64,
    record_end: u64,
    value: records::NativeValue,
) -> Result<Range<u64>, AppError> {
    let start = base
        .checked_add(value.span.start)
        .ok_or(AppError::CursorExpired)?;
    let end = base
        .checked_add(value.span.end)
        .ok_or(AppError::CursorExpired)?;
    if !value.string || start >= end || end > record_end {
        return Err(AppError::CursorExpired);
    }
    Ok(start..end)
}

#[cfg(test)]
mod tests;
