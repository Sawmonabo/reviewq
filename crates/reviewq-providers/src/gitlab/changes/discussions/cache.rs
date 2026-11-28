use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::changes::{Discussion, DiscussionDraft, DiscussionScope};
use reviewq_app::wait::{Place, Places, Turn};
use reviewq_domain::identity::{EventCursor, MrIdentity};
use tokio::sync::{OnceCell, OwnedSemaphorePermit};
use tokio::time::Instant as Deadline;

const OBSERVATIONS: usize = 5;
const RETAINED_BYTES: usize = 8 * 1024 * 1024 / OBSERVATIONS;
const RETAINED_PAGES: usize = 64;
const LIFETIME: Duration = Duration::from_secs(45);

pub(in crate::gitlab) struct DiscussionCache {
    entries: Mutex<VecDeque<Entry>>,
    /// One per observation in use, so a caller holding one always finds an observation to
    /// replace when it needs a new one.
    uses: Places,
}

impl Default for DiscussionCache {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
            uses: Places::new(OBSERVATIONS),
        }
    }
}

struct Entry {
    source: MrIdentity,
    mr_id: Option<u64>,
    revision: String,
    epoch: EventCursor,
    journal_head: EventCursor,
    observation: Arc<Observation>,
    /// Held by every caller using the observation; it is in use while its place is held.
    place: Weak<OwnedSemaphorePermit>,
}

pub(super) struct Observation {
    pub(super) id: String,
    opened: Instant,
    expires: Instant,
    pages: Mutex<VecDeque<PageSlot>>,
    /// One per page being read from GitLab, so a reader holding one always finds a page to
    /// replace.
    page_uses: Places,
    pub(super) drafts: OnceCell<Drafts>,
}

type PageCell = Arc<OnceCell<Option<Page>>>;

struct PageSlot {
    address: (u32, u16),
    value: PageCell,
    /// Held by the callers reading the page from GitLab or waiting for it. A page already
    /// read needs no place: dropping it from the list never affects a caller holding it.
    place: Weak<OwnedSemaphorePermit>,
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
    /// The observation of `source`'s discussions in `scope`, with its place among
    /// `OBSERVATIONS`. A caller of an observation in use joins its place; a caller that needs
    /// an observation no one is using waits until `deadline` for a place when every place is
    /// in use, and joins the observation if another caller opens it meanwhile. Keep the
    /// place while using the observation.
    pub(super) async fn observe(
        &self,
        source: &MrIdentity,
        mr_id: Option<u64>,
        scope: &DiscussionScope,
        deadline: Deadline,
    ) -> Result<(Place, Arc<Observation>), AppError> {
        let mut turn = self.uses.turn(deadline);
        loop {
            if let Some(found) = self.find_or_open(source, mr_id, scope, &mut turn)? {
                return Ok(found);
            }
            turn.wait().await?;
        }
    }

    /// The observation and its place, or `None` when it needs a place `turn` does not have.
    fn find_or_open(
        &self,
        source: &MrIdentity,
        mr_id: Option<u64>,
        scope: &DiscussionScope,
        turn: &mut Turn<'_>,
    ) -> Result<Option<(Place, Arc<Observation>)>, AppError> {
        let mut entries = self.entries.lock().map_err(|_| AppError::Internal)?;
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
            if same {
                let mut entry = entries.remove(index).ok_or(AppError::Internal)?;
                let place = entry.place.upgrade().or_else(|| {
                    let place = turn.take()?;
                    entry.place = Arc::downgrade(&place);
                    Some(place)
                });
                if place.is_some() {
                    entry.journal_head = entry.journal_head.max(scope.journal_head);
                }
                let observation = entry.observation.clone();
                entries.push_back(entry);
                return Ok(place.map(|place| (place, observation)));
            }
        }
        if scope.observation.is_some() {
            return Err(AppError::CursorExpired);
        }
        let Some(place) = turn.take() else {
            return Ok(None);
        };
        // A newer scope replaces the source's observation; its callers keep their own copy.
        entries.retain(|entry| entry.source != *source);
        while entries.len() >= OBSERVATIONS {
            // The caller's place leaves at most `OBSERVATIONS - 1` observations in use.
            let index = entries
                .iter()
                .position(|entry| entry.place.strong_count() == 0)
                .ok_or(AppError::Internal)?;
            entries.remove(index);
        }
        let observation = Arc::new(Observation {
            id: uuid::Uuid::new_v4().to_string(),
            opened: now,
            expires: now + LIFETIME,
            pages: Mutex::default(),
            page_uses: Places::new(RETAINED_PAGES),
            drafts: OnceCell::const_new(),
        });
        entries.push_back(Entry {
            source: source.clone(),
            mr_id,
            revision: scope.revision.clone(),
            epoch: scope.epoch,
            journal_head: scope.journal_head,
            observation: observation.clone(),
            place: Arc::downgrade(&place),
        });
        Ok(Some((place, observation)))
    }

    pub(super) fn check(
        &self,
        source: &MrIdentity,
        scope: &DiscussionScope,
        observation: &Arc<Observation>,
    ) -> Result<(), AppError> {
        let entries = self.entries.lock().map_err(|_| AppError::Internal)?;
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
        let mut entries = self.entries.lock().map_err(|_| AppError::Internal)?;
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
    /// The slot of one page. A page already read answers at once, and a reader of a page
    /// being read joins its readers' place; only a reader that starts a page waits until
    /// `deadline` for one of `RETAINED_PAGES` places when every place is reading a page, and
    /// joins the page if another reader starts it meanwhile. Keep the place while using the
    /// slot.
    pub(super) async fn page(
        &self,
        page: u32,
        limit: u16,
        deadline: Deadline,
    ) -> Result<(Option<Place>, PageCell), AppError> {
        let mut turn = self.page_uses.turn(deadline);
        loop {
            if let Some(found) = self.page_slot((page, limit), &mut turn)? {
                return Ok(found);
            }
            turn.wait().await?;
        }
    }

    /// The page's slot and place, or `None` when it needs a place `turn` does not have.
    fn page_slot(
        &self,
        address: (u32, u16),
        turn: &mut Turn<'_>,
    ) -> Result<Option<(Option<Place>, PageCell)>, AppError> {
        let mut pages = self.pages.lock().map_err(|_| AppError::Internal)?;
        pages.retain(|item| item.value.initialized() || item.place.strong_count() > 0);
        if let Some(index) = pages.iter().position(|item| item.address == address) {
            let mut item = pages.remove(index).ok_or(AppError::Internal)?;
            let place = if item.value.initialized() {
                Some(None)
            } else if let Some(place) = item.place.upgrade() {
                Some(Some(place))
            } else {
                turn.take().map(|place| {
                    item.place = Arc::downgrade(&place);
                    Some(place)
                })
            };
            let slot = item.value.clone();
            pages.push_back(item);
            return Ok(place.map(|place| (place, slot)));
        }
        let Some(place) = turn.take() else {
            return Ok(None);
        };
        if pages.len() == RETAINED_PAGES {
            // The caller's place leaves at most `RETAINED_PAGES - 1` pages being read.
            let index = pages
                .iter()
                .position(|item| item.place.strong_count() == 0)
                .ok_or(AppError::Internal)?;
            pages.remove(index);
        }
        let slot = Arc::new(OnceCell::const_new());
        pages.push_back(PageSlot {
            address,
            value: slot.clone(),
            place: Arc::downgrade(&place),
        });
        Ok(Some((Some(place), slot)))
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
