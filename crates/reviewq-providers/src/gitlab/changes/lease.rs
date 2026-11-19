use std::collections::VecDeque;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::changes::{PatchBytes, PatchContent, PatchFile, PatchFileSelection, SourcePage};
use reviewq_domain::identity::MrIdentity;
use tokio::sync::{Mutex, OnceCell};

const MAX_LEASES: usize = 4;
const INACTIVITY: Duration = Duration::from_secs(30);
pub(super) const MEMORY_LIMIT: usize = 2 * 1024 * 1024;

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
type IndexSlot = OnceCell<SourcePage<PatchFile>>;

struct Lease {
    key: Key,
    slot: Slot,
    used_at: Instant,
}

pub(in crate::gitlab) struct Leases {
    pub(super) profile: PathBuf,
    entries: Arc<Mutex<VecDeque<Lease>>>,
    scans: Mutex<Vec<(IndexKey, Weak<IndexSlot>)>>,
}

pub(super) enum Body {
    Memory(Vec<u8>),
    File(Arc<File>),
}

impl Leases {
    pub(in crate::gitlab) fn new(profile: PathBuf) -> Self {
        let entries = Arc::new(Mutex::new(VecDeque::<Lease>::new()));
        let expiry = Arc::downgrade(&entries);
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
            }
        });
        Self {
            profile,
            entries,
            scans: Mutex::new(Vec::new()),
        }
    }

    pub(super) async fn index(&self, key: IndexKey) -> Result<Arc<IndexSlot>, AppError> {
        let mut scans = self.scans.lock().await;
        scans.retain(|(_, pending)| pending.strong_count() > 0);
        if let Some(active) = scans
            .iter()
            .find(|(existing, _)| existing == &key)
            .and_then(|(_, pending)| pending.upgrade())
        {
            return Ok(active);
        }
        if scans.len() >= MAX_LEASES {
            return Err(AppError::Busy);
        }
        let slot = Arc::new(OnceCell::new());
        scans.push((key, Arc::downgrade(&slot)));
        Ok(slot)
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
                        file.read_exact_at(&mut bytes, offset as u64)
                            .map_err(|_| AppError::Unavailable)?;
                        Ok(bytes)
                    })
                    .await
                    .map_err(|_| AppError::Unavailable)?
                }
            }
        })
    }
}
