use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::changes::{Discussion, DiscussionDraft, DiscussionScope};
use reviewq_domain::identity::{EventCursor, MrIdentity};
use tokio::sync::OnceCell;

const OBSERVATIONS: usize = 5;
const RETAINED_BYTES: usize = 8 * 1024 * 1024 / OBSERVATIONS;
const RETAINED_PAGES: usize = 64;
const LIFETIME: Duration = Duration::from_secs(45);

#[derive(Default)]
pub(in crate::gitlab) struct DiscussionCache(Mutex<VecDeque<Entry>>);

struct Entry {
    source: MrIdentity,
    mr_id: Option<u64>,
    revision: String,
    epoch: EventCursor,
    journal_head: EventCursor,
    observation: Arc<Observation>,
}

pub(super) struct Observation {
    pub(super) id: String,
    opened: Instant,
    expires: Instant,
    pages: Mutex<VecDeque<PageSlot>>,
    pub(super) drafts: OnceCell<Drafts>,
}

struct PageSlot {
    address: (u32, u16),
    value: Arc<OnceCell<Option<Page>>>,
}

pub(super) struct Page {
    pub(super) items: Arc<[Discussion]>,
    pub(super) next: Option<u32>,
    pub(super) weight: usize,
    pub(super) limited: bool,
    pub(super) fingerprint: Vec<Option<[u8; 32]>>,
    pub(super) started: Instant,
}

pub(super) struct Drafts {
    pub(super) items: Arc<[DiscussionDraft]>,
    pub(super) weight: usize,
    pub(super) limited: bool,
    pub(super) fingerprint: [u8; 32],
}

impl DiscussionCache {
    pub(super) fn observe(
        &self,
        source: &MrIdentity,
        mr_id: Option<u64>,
        scope: &DiscussionScope,
    ) -> Result<Arc<Observation>, AppError> {
        let mut entries = self.0.lock().map_err(|_| AppError::Internal)?;
        let now = Instant::now();
        entries.retain(|entry| entry.observation.expires > now);
        if let Some(index) = entries.iter().position(|entry| entry.source == *source) {
            let existing = &entries[index];
            let same = existing.epoch == scope.epoch
                && existing.mr_id == mr_id
                && existing.revision == scope.revision;
            if (!same && existing.journal_head > scope.journal_head)
                || (existing.epoch > scope.epoch && scope.journal_head <= existing.journal_head)
                || scope
                    .observation
                    .as_ref()
                    .is_some_and(|id| !same || id != &existing.observation.id)
            {
                return Err(AppError::CursorExpired);
            }
            let mut entry = entries.remove(index).ok_or(AppError::Internal)?;
            if same {
                entry.journal_head = entry.journal_head.max(scope.journal_head);
                let observation = entry.observation.clone();
                entries.push_back(entry);
                return Ok(observation);
            }
        }
        if scope.observation.is_some() {
            return Err(AppError::CursorExpired);
        }
        while entries.len() >= OBSERVATIONS {
            let index = entries
                .iter()
                .position(|entry| Arc::strong_count(&entry.observation) == 1)
                .ok_or(AppError::Busy)?;
            entries.remove(index);
        }
        let observation = Arc::new(Observation {
            id: uuid::Uuid::new_v4().to_string(),
            opened: now,
            expires: now + LIFETIME,
            pages: Mutex::default(),
            drafts: OnceCell::const_new(),
        });
        entries.push_back(Entry {
            source: source.clone(),
            mr_id,
            revision: scope.revision.clone(),
            epoch: scope.epoch,
            journal_head: scope.journal_head,
            observation: observation.clone(),
        });
        Ok(observation)
    }

    pub(super) fn check(
        &self,
        source: &MrIdentity,
        scope: &DiscussionScope,
        observation: &Arc<Observation>,
    ) -> Result<(), AppError> {
        let entries = self.0.lock().map_err(|_| AppError::Internal)?;
        if observation.expires > Instant::now()
            && entries.iter().any(|entry| {
                &entry.source == source
                    && entry.epoch == scope.epoch
                    && Arc::ptr_eq(&entry.observation, observation)
            })
        {
            Ok(())
        } else {
            Err(AppError::CursorExpired)
        }
    }

    pub(super) fn invalidate_if_changed(
        &self,
        source: &MrIdentity,
        started: Instant,
        current: &[Option<[u8; 32]>],
    ) -> Result<bool, AppError> {
        let mut entries = self.0.lock().map_err(|_| AppError::Internal)?;
        let Some(index) = entries.iter().position(|entry| entry.source == *source) else {
            return Ok(false);
        };
        let scope = &entries[index].observation;
        if scope.opened > started {
            return Ok(false);
        }
        let changed = scope
            .pages
            .lock()
            .map_err(|_| AppError::Internal)?
            .iter()
            .any(|slot| {
                let Some(read) = slot
                    .value
                    .get()
                    .and_then(Option::as_ref)
                    .filter(|read| read.started <= started)
                else {
                    return false;
                };
                let (page, limit) = slot.address;
                let start = (page as usize - 1) * usize::from(limit);
                let end = (start + usize::from(limit)).min(current.len());
                current.get(start..end) != Some(read.fingerprint.as_slice())
                    || read.next.is_some() != (end < current.len())
            });
        if changed {
            entries.remove(index);
        }
        Ok(changed)
    }
}

impl Observation {
    pub(super) fn page(
        &self,
        page: u32,
        limit: u16,
    ) -> Result<Arc<OnceCell<Option<Page>>>, AppError> {
        let mut pages = self.pages.lock().map_err(|_| AppError::Internal)?;
        pages.retain(|item| item.value.initialized() || Arc::strong_count(&item.value) > 1);
        let address = (page, limit);
        if let Some(index) = pages.iter().position(|item| item.address == address) {
            let item = pages.remove(index).ok_or(AppError::Internal)?;
            let slot = item.value.clone();
            pages.push_back(item);
            return Ok(slot);
        }
        if pages.len() == RETAINED_PAGES {
            let index = pages
                .iter()
                .position(|item| Arc::strong_count(&item.value) == 1)
                .ok_or(AppError::Busy)?;
            pages.remove(index);
        }
        let slot = Arc::new(OnceCell::const_new());
        pages.push_back(PageSlot {
            address,
            value: slot.clone(),
        });
        Ok(slot)
    }

    pub(super) fn trim(&self) -> Result<(), AppError> {
        let mut pages = self.pages.lock().map_err(|_| AppError::Internal)?;
        let drafts = self.drafts.get().map_or(0, |drafts| drafts.weight);
        let mut remaining = RETAINED_BYTES.saturating_sub(drafts);
        for index in (0..pages.len()).rev() {
            if let Some(value) = pages[index].value.get().and_then(Option::as_ref) {
                if value.weight <= remaining {
                    remaining -= value.weight;
                } else {
                    pages.remove(index);
                }
            }
        }
        Ok(())
    }
}

pub(super) fn discussion_bytes(thread: &Discussion) -> usize {
    size_of::<Discussion>()
        + thread.id.capacity()
        + anchor_bytes(thread.anchor.as_ref())
        + thread.notes.capacity() * size_of::<reviewq_app::changes::DiscussionNote>()
        + thread
            .notes
            .iter()
            .map(|note| note.body.capacity() + note.author.capacity())
            .sum::<usize>()
}

pub(super) fn draft_bytes(note: &DiscussionDraft) -> usize {
    size_of::<DiscussionDraft>()
        + note.body.capacity()
        + note.author.capacity()
        + note.parent.as_ref().map_or(0, String::capacity)
        + anchor_bytes(note.anchor.as_ref())
}

fn anchor_bytes(anchor: Option<&reviewq_app::changes::DiscussionAnchor>) -> usize {
    anchor.map_or(0, |a| {
        a.old_path.capacity()
            + a.new_path.capacity()
            + a.base_sha.as_str().len()
            + a.start_sha.as_str().len()
            + a.head_sha.as_str().len()
    })
}
