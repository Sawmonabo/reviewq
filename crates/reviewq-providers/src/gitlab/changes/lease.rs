use std::collections::VecDeque;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::changes::{
    PatchBytes, PatchContent, PatchFile, PatchFileSelection, PatchPagePreview, PatchSourceTarget,
    PatchSourceWindow, SourcePage,
};
use reviewq_app::wait::{self, Place, Places, Turn};
use reviewq_domain::identity::MrIdentity;
use tokio::sync::{Mutex, OnceCell, OwnedSemaphorePermit, Semaphore};

use super::source::{SourceIndex, SourceRead};

mod scratch;
pub(super) use scratch::{ScratchFile, ScratchKey};

const MAX_LEASES: usize = 4;
const MAX_SOURCE_READS: usize = 12;
const INACTIVITY: Duration = Duration::from_secs(30);
pub(super) const MEMORY_LIMIT: usize = 2 * 1024 * 1024;
pub(super) const INLINE_LIMIT: usize = 128 * 1024;

#[derive(Eq, PartialEq)]
pub(super) struct Key {
    pub source: MrIdentity,
    pub revision: String,
    pub file: String,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct IndexKey {
    pub source: MrIdentity,
    pub revision: String,
    pub offset: (u32, u16),
    pub preferred: Option<PatchFileSelection>,
}

type Slot = Arc<OnceCell<PatchBytes>>;
pub(super) struct IndexedPage {
    pub page: SourcePage<PatchFile>,
    pub small: Vec<(usize, Arc<Body>)>,
    pub selected: Option<(usize, Weak<Body>)>,
}

impl IndexedPage {
    pub(super) fn for_reader(
        &self,
        preview: Option<PatchPagePreview>,
    ) -> Result<SourcePage<PatchFile>, AppError> {
        let mut page = self.page.clone();
        let Some(preview) = preview else {
            return Ok(page);
        };
        if matches!(preview, PatchPagePreview::Selected) {
            if let Some((index, body)) = &self.selected
                && let Some(body) = body.upgrade()
                && let Some(bytes) = body.memory()
            {
                page.items[*index].preview = Some(
                    std::str::from_utf8(bytes)
                        .map_err(|_| AppError::Unavailable)?
                        .to_owned(),
                );
            }
            return Ok(page);
        };
        // Use only bytes from this verified scan. Preparing adjacent files never performs I/O.
        let mut available = 64 * 1024;
        let len = page.items.len();
        for (index, body) in &self.small {
            let distance = match preview {
                PatchPagePreview::Selected => unreachable!("already returned the selected preview"),
                PatchPagePreview::Start => *index,
                PatchPagePreview::End => len - 1 - index,
            };
            let file = &mut page.items[*index];
            if distance >= 3
                || !file
                    .metrics
                    .as_ref()
                    .is_some_and(|m| m.max_line_bytes <= 16 * 1024 && m.line_count <= 20_000)
            {
                continue;
            }
            if let Some(bytes) = body.memory()
                && !bytes.is_empty()
                && bytes.len() <= available
            {
                let value = std::str::from_utf8(bytes).map_err(|_| AppError::Unavailable)?;
                file.preview = Some(value.to_owned());
                available -= bytes.len();
            }
        }
        Ok(page)
    }
}

type IndexSlot = OnceCell<IndexedPage>;

struct IndexLease {
    key: IndexKey,
    slot: Arc<IndexSlot>,
    /// Held by the callers building the page or waiting for it; an index page is in use
    /// while its place is held. A built page needs no place: dropping it from the list never
    /// affects a caller already reading it.
    place: Weak<OwnedSemaphorePermit>,
    used_at: Instant,
}

struct Lease {
    key: Key,
    slot: Slot,
    used_at: Instant,
}

#[derive(Clone)]
pub(in crate::gitlab) struct Leases {
    pub(in crate::gitlab) profile: PathBuf,
    entries: Arc<Mutex<VecDeque<Lease>>>,
    scans: Arc<Mutex<VecDeque<IndexLease>>>,
    pub(super) flights: super::flights::Flights,
    pub(super) source_reads: Arc<Semaphore>,
    /// One per file-index page being built, so a caller holding one always finds a page to
    /// reuse.
    scan_uses: Arc<Places>,
}

pub(super) struct Body {
    storage: Storage,
    index: OnceLock<Arc<SourceIndex>>,
}

pub(super) enum Storage {
    Memory(Vec<u8>),
    File(Arc<ScratchFile>),
    Window { start: usize, bytes: Vec<u8> },
}

impl Body {
    pub(super) fn new(storage: Storage, index: Option<SourceIndex>) -> Self {
        Self {
            storage,
            index: index.map_or_else(OnceLock::new, |value| OnceLock::from(Arc::new(value))),
        }
    }

    pub(super) fn memory(&self) -> Option<&Vec<u8>> {
        match &self.storage {
            Storage::Memory(bytes) => Some(bytes),
            _ => None,
        }
    }

    pub(super) fn is_window(&self) -> bool {
        matches!(self.storage, Storage::Window { .. })
    }

    pub(super) fn index(&self) -> Option<&Arc<SourceIndex>> {
        self.index.get()
    }
}

impl Leases {
    pub(in crate::gitlab) fn new(profile: PathBuf) -> Self {
        let entries = Arc::new(Mutex::new(VecDeque::<Lease>::new()));
        let scans = Arc::new(Mutex::new(VecDeque::<IndexLease>::new()));
        let expiry = Arc::downgrade(&entries);
        let scan_expiry = Arc::downgrade(&scans);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let Some(entries) = expiry.upgrade() else {
                    break;
                };
                entries
                    .lock()
                    .await
                    .retain(|entry| entry.used_at.elapsed() < INACTIVITY || entry.in_use());
                if let Some(scans) = scan_expiry.upgrade() {
                    scans.lock().await.retain(|scan| {
                        scan.used_at.elapsed() < INACTIVITY || scan.place.strong_count() > 0
                    });
                }
            }
        });
        Self {
            profile,
            entries,
            scans,
            flights: super::flights::Flights::default(),
            source_reads: Arc::new(Semaphore::new(MAX_SOURCE_READS)),
            scan_uses: Arc::new(Places::new(MAX_LEASES)),
        }
    }

    pub(super) fn refetch_only(&self) -> Self {
        // A leased fallback shares in-flight provider reads but has no strong
        // reference back to the entries that own it.
        Self {
            profile: self.profile.clone(),
            entries: Arc::default(),
            scans: Arc::default(),
            flights: self.flights.clone(),
            source_reads: self.source_reads.clone(),
            scan_uses: Arc::new(Places::new(MAX_LEASES)),
        }
    }

    pub(super) async fn discard_stale(&self, key: &Key, etag: &str) {
        let stale = |file: &PatchFile| {
            file.id == key.file && file.metrics.as_ref().is_some_and(|data| data.etag == etag)
        };
        self.entries.lock().await.retain(|entry| {
            &entry.key != key || entry.slot.get().is_none_or(|patch| !stale(&patch.file))
        });
        self.scans.lock().await.retain(|scan| {
            scan.key.source != key.source
                || scan.key.revision != key.revision
                || scan
                    .slot
                    .get()
                    .is_none_or(|indexed| !indexed.page.items.iter().any(stale))
        });
    }

    /// The slot of one file-index page. A built page answers at once, and a caller of a page
    /// being built joins its builders' place; only a caller that starts a page waits for one
    /// of `MAX_LEASES` places when every place is building a page, and joins the page if
    /// another caller starts it meanwhile. Keep the place while using the slot.
    pub(super) async fn index(
        &self,
        key: IndexKey,
    ) -> Result<(Option<Place>, Arc<IndexSlot>), AppError> {
        let mut turn = self.scan_uses.turn(wait::deadline());
        loop {
            if let Some(found) = self.index_slot(&key, &mut turn).await? {
                return Ok(found);
            }
            turn.wait().await?;
        }
    }

    /// The page's slot and place, or `None` when it needs a place `turn` does not have.
    async fn index_slot(
        &self,
        key: &IndexKey,
        turn: &mut Turn<'_>,
    ) -> Result<Option<(Option<Place>, Arc<IndexSlot>)>, AppError> {
        let mut scans = self.scans.lock().await;
        // The caller checks the current GitLab refs before every lookup. Retain only
        // bounded, fully verified REST pages under their original request identity.
        scans.retain(|scan| {
            scan.place.strong_count() > 0
                || (scan.slot.initialized()
                    && scan.used_at.elapsed() < INACTIVITY
                    && (scan.key.source != key.source || scan.key.revision == key.revision))
        });
        if let Some(position) = scans.iter().position(|scan| scan.key == *key) {
            let mut scan = scans.remove(position).ok_or(AppError::Internal)?;
            scan.used_at = Instant::now();
            let place = if scan.slot.initialized() {
                Some(None)
            } else if let Some(place) = scan.place.upgrade() {
                Some(Some(place))
            } else {
                // Its builders left before it was built; this caller builds it again.
                turn.take().map(|place| {
                    scan.place = Arc::downgrade(&place);
                    Some(place)
                })
            };
            let slot = scan.slot.clone();
            scans.push_back(scan);
            return Ok(place.map(|place| (place, slot)));
        }
        let Some(place) = turn.take() else {
            return Ok(None);
        };
        if scans.len() == MAX_LEASES {
            // The caller's place leaves at most `MAX_LEASES - 1` pages being built by others.
            let unused = scans.iter().position(|scan| scan.place.strong_count() == 0);
            scans.remove(unused.ok_or(AppError::Internal)?);
        }
        let slot = Arc::new(OnceCell::new());
        scans.push_back(IndexLease {
            key: key.clone(),
            slot: slot.clone(),
            place: Arc::downgrade(&place),
            used_at: Instant::now(),
        });
        Ok(Some((Some(place), slot)))
    }

    // An index page already verified the neighboring bytes against the same refs.
    pub(super) async fn indexed_patch(&self, key: &Key) -> Option<PatchBytes> {
        let scans = self.scans.lock().await;
        for scan in scans.iter().rev().filter(|scan| {
            scan.key.source == key.source
                && scan.key.revision == key.revision
                && scan.used_at.elapsed() < INACTIVITY
        }) {
            let Some(index) = scan.slot.get() else {
                continue;
            };
            for (ordinal, body) in &index.small {
                let file = &index.page.items[*ordinal];
                if file.id == key.file {
                    return Some(PatchBytes {
                        revision: index.page.revision.clone(),
                        file: file.clone(),
                        body: body.clone(),
                    });
                }
            }
        }
        None
    }

    pub(super) async fn leased_patch(&self, key: &Key) -> Option<PatchBytes> {
        let mut entries = self.entries.lock().await;
        let lease = entries.iter_mut().find(|entry| {
            &entry.key == key && (entry.used_at.elapsed() < INACTIVITY || entry.in_use())
        })?;
        let patch = lease.slot.get()?;
        if !patch.body.retained() {
            return None;
        }
        lease.used_at = Instant::now();
        Some(patch.clone())
    }

    pub(super) async fn slot(&self, key: Key) -> Result<Slot, AppError> {
        let mut entries = self.entries.lock().await;
        entries.retain(|entry| {
            entry.in_use()
                || (entry.used_at.elapsed() < INACTIVITY
                    && (entry.key.source != key.source || entry.key.revision == key.revision))
        });
        if let Some(position) = entries.iter().position(|entry| entry.key == key) {
            let mut entry = entries.remove(position).ok_or(AppError::Internal)?;
            let slot = entry.slot.clone();
            entry.used_at = Instant::now();
            entries.push_back(entry);
            return Ok(slot);
        }
        if entries.len() == MAX_LEASES {
            let unused = entries.iter().position(|entry| !entry.in_use());
            entries.remove(unused.ok_or(AppError::Busy)?);
        }
        let slot = Arc::new(OnceCell::new());
        entries.push_back(Lease {
            key,
            slot: slot.clone(),
            used_at: Instant::now(),
        });
        Ok(slot)
    }

    pub(super) async fn prepare(
        &self,
        source: &MrIdentity,
        revision: &str,
        preferred: Option<u32>,
    ) {
        let mut entries = self.entries.lock().await;
        entries.retain(|entry| {
            entry.in_use()
                || (entry.used_at.elapsed() < INACTIVITY
                    && (entry.key.source != *source || entry.key.revision == revision))
        });
        if entries.len() == MAX_LEASES
            && entries.iter().all(|entry| {
                entry.key.source != *source
                    || entry.key.revision != revision
                    || super::ordinal(&entry.key.file) != preferred
            })
            && let Some(unused) = entries.iter().position(|entry| !entry.in_use())
        {
            entries.remove(unused);
        }
    }
}

impl Lease {
    fn in_use(&self) -> bool {
        Arc::strong_count(&self.slot) > 1
            || self
                .slot
                .get()
                .is_some_and(|lease| Arc::strong_count(&lease.body) > 1)
    }
}

impl PatchContent for Body {
    fn retained(&self) -> bool {
        !self.is_window()
    }

    fn covers(&self, offset: usize, length: usize) -> bool {
        match &self.storage {
            Storage::Memory(bytes) => length <= bytes.len().saturating_sub(offset),
            Storage::File(_) => true,
            Storage::Window { start, bytes } => offset
                .checked_sub(*start)
                .is_some_and(|offset| offset <= bytes.len() && length <= bytes.len() - offset),
        }
    }

    fn read(&self, offset: usize, length: usize) -> AppFuture<'_, Vec<u8>> {
        Box::pin(async move {
            match &self.storage {
                Storage::Memory(bytes) => {
                    let end = offset.checked_add(length).ok_or(AppError::Internal)?;
                    bytes
                        .get(offset..end)
                        .map(<[u8]>::to_vec)
                        .ok_or(AppError::Internal)
                }
                Storage::File(owner) => {
                    let file = owner.clone();
                    tokio::task::spawn_blocking(move || {
                        let mut bytes = vec![0; length];
                        file.file
                            .read_exact_at(&mut bytes, offset as u64)
                            .map_err(|_| AppError::Unavailable)?;
                        Ok(bytes)
                    })
                    .await
                    .map_err(|_| AppError::Unavailable)?
                }
                Storage::Window { start, bytes } => {
                    let first = offset.checked_sub(*start).ok_or(AppError::Internal)?;
                    let end = first.checked_add(length).ok_or(AppError::Internal)?;
                    bytes
                        .get(first..end)
                        .map(<[u8]>::to_vec)
                        .ok_or(AppError::Internal)
                }
            }
        })
    }

    fn source_window(&self, target: PatchSourceTarget) -> AppFuture<'_, PatchSourceWindow> {
        Box::pin(async move {
            let index = match self.memory() {
                Some(bytes) => self
                    .index
                    .get_or_init(|| Arc::new(SourceIndex::memory(bytes))),
                None => self.index.get().ok_or(AppError::Internal)?,
            };
            let mut reader = self;
            index.window(&mut reader, target).await
        })
    }
}

impl SourceRead for &Body {
    fn read(&mut self, offset: usize, length: usize) -> AppFuture<'_, Vec<u8>> {
        PatchContent::read(*self, offset, length)
    }
}

#[cfg(test)]
mod tests;
