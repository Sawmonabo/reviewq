//! Short-lived layouts and sparse decoded-to-native offsets. The native file stays authoritative.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::history::document::{Index, MAX_INDEX_BYTES, Query, SourcePages, Window};
use reviewq_app::history::{FileContentRef, search::SourceObservation};
use reviewq_domain::identity::ReportHash;
use reviewq_domain::session::SessionTarget;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use super::{file, locator::Source, native_string::NativeString, records, search};

const MAX_DOCUMENTS: usize = 3;
const RETENTION: Duration = Duration::from_secs(30);

type Key = (SessionTarget, FileContentRef);
type Handle = Arc<AsyncMutex<Option<Prepared>>>;

#[derive(Default)]
pub(super) struct Documents {
    active: Arc<Mutex<HashMap<Key, Slot>>>,
    collect: AtomicBool,
}

struct Slot {
    touched: Instant,
    handle: Handle,
}

impl Documents {
    pub(super) fn slot(
        &self,
        target: SessionTarget,
        reference: FileContentRef,
    ) -> Result<Handle, AppError> {
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
        let key = (target, reference);
        if !active.contains_key(&key) && active.len() >= MAX_DOCUMENTS {
            let oldest = active
                .iter()
                .filter(|(_, slot)| Arc::strong_count(&slot.handle) == 1)
                .min_by_key(|(_, slot)| slot.touched)
                .map(|(key, _)| key.clone())
                .ok_or(AppError::Busy)?;
            active.remove(&oldest);
        }
        let slot = active.entry(key).or_insert_with(|| Slot {
            touched: Instant::now(),
            handle: Arc::default(),
        });
        slot.touched = Instant::now();
        Ok(slot.handle.clone())
    }
}

fn alive(slot: &Slot) -> bool {
    slot.touched.elapsed() < RETENTION || Arc::strong_count(&slot.handle) > 1
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
    let mut pages = SourcePages::default();
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
