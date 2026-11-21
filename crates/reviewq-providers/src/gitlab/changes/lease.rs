use std::collections::VecDeque;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::changes::{
    PatchBytes, PatchContent, PatchFile, PatchFileSelection, PatchPageEdge, SourcePage,
};
use reviewq_domain::identity::MrIdentity;
use tokio::sync::{Mutex, OnceCell};

mod scratch;
pub(super) use scratch::{ScratchFile, ScratchKey};

const MAX_LEASES: usize = 4;
const INACTIVITY: Duration = Duration::from_secs(30);
pub(super) const MEMORY_LIMIT: usize = 2 * 1024 * 1024;
pub(super) const INLINE_LIMIT: usize = 128 * 1024;

#[derive(Eq, PartialEq)]
pub(super) struct Key {
    pub source: MrIdentity,
    pub revision: String,
    pub file: String,
}

#[derive(Eq, PartialEq)]
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
    pub selected: Option<(usize, Arc<Body>)>,
}

impl IndexedPage {
    pub(super) fn for_reader(
        &self,
        edge: Option<PatchPageEdge>,
    ) -> Result<SourcePage<PatchFile>, AppError> {
        let mut page = self.page.clone();
        let Some(edge) = edge else {
            if let Some((index, body)) = &self.selected
                && let Body::Memory(bytes) = body.as_ref()
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
            let distance = match edge {
                PatchPageEdge::Start => *index,
                PatchPageEdge::End => len - 1 - index,
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
            if let Body::Memory(bytes) = body.as_ref()
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
    used_at: Instant,
}

struct Lease {
    key: Key,
    slot: Slot,
    used_at: Instant,
}

pub(in crate::gitlab) struct Leases {
    pub(in crate::gitlab) profile: PathBuf,
    entries: Arc<Mutex<VecDeque<Lease>>>,
    scans: Arc<Mutex<VecDeque<IndexLease>>>,
}

pub(super) enum Body {
    Memory(Vec<u8>),
    File(Arc<ScratchFile>),
    Window { start: usize, bytes: Vec<u8> },
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
                        scan.used_at.elapsed() < INACTIVITY || Arc::strong_count(&scan.slot) > 1
                    });
                }
            }
        });
        Self {
            profile,
            entries,
            scans,
        }
    }

    pub(super) async fn index(&self, key: IndexKey) -> Result<Arc<IndexSlot>, AppError> {
        let mut scans = self.scans.lock().await;
        // The caller checks the current GitLab refs before every lookup. Retain only
        // bounded, fully verified REST pages under their original request identity.
        scans.retain(|scan| {
            Arc::strong_count(&scan.slot) > 1
                || (scan.slot.initialized()
                    && scan.used_at.elapsed() < INACTIVITY
                    && (scan.key.source != key.source || scan.key.revision == key.revision))
        });
        if let Some(position) = scans.iter().position(|scan| scan.key == key) {
            let mut scan = scans.remove(position).ok_or(AppError::Internal)?;
            let slot = scan.slot.clone();
            scan.used_at = Instant::now();
            scans.push_back(scan);
            return Ok(slot);
        }
        if scans.len() == MAX_LEASES {
            let unused = scans
                .iter()
                .position(|scan| Arc::strong_count(&scan.slot) == 1);
            scans.remove(unused.ok_or(AppError::Busy)?);
        }
        let slot = Arc::new(OnceCell::new());
        scans.push_back(IndexLease {
            key,
            slot: slot.clone(),
            used_at: Instant::now(),
        });
        Ok(slot)
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
        let patch = lease.slot.get()?.clone();
        lease.used_at = Instant::now();
        Some(patch)
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
    fn covers(&self, offset: usize, length: usize) -> bool {
        match self {
            Self::Memory(bytes) => length <= bytes.len().saturating_sub(offset),
            Self::File(_) => true,
            Self::Window { start, bytes } => offset
                .checked_sub(*start)
                .is_some_and(|offset| offset <= bytes.len() && length <= bytes.len() - offset),
        }
    }

    fn read(&self, offset: usize, length: usize) -> AppFuture<'_, Vec<u8>> {
        Box::pin(async move {
            match self {
                Self::Memory(bytes) => {
                    let end = offset.checked_add(length).ok_or(AppError::Internal)?;
                    bytes
                        .get(offset..end)
                        .map(<[u8]>::to_vec)
                        .ok_or(AppError::Internal)
                }
                Self::File(owner) => {
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
                Self::Window { start, bytes } => {
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
}
