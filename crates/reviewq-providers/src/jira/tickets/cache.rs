use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::sources::{CommentOrder, TicketComment, TicketCommentPage, TicketOpening};
use reviewq_domain::identity::TicketKey;
use tokio::sync::OnceCell;

const MAX_ENTRIES: usize = 10;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const LIFETIME: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(in crate::jira) struct Cache {
    entries: Mutex<VecDeque<Entry>>,
}

#[derive(Eq, PartialEq)]
struct Key {
    ticket: TicketKey,
    estimate_fields: Vec<String>,
    limit: u16,
    order: CommentOrder,
}

struct Entry {
    key: Key,
    load: Arc<OnceCell<Cached>>,
}

struct Cached {
    detail: TicketOpening,
    expires: Instant,
    bytes: usize,
}

impl Cache {
    pub(in crate::jira) async fn read<F, Check>(
        &self,
        ticket: TicketKey,
        estimate_fields: Vec<String>,
        limit: u16,
        order: CommentOrder,
        fetch: impl FnOnce() -> F,
        recheck: impl FnOnce(u64) -> Check,
    ) -> Result<TicketOpening, AppError>
    where
        F: Future<Output = Result<TicketOpening, AppError>>,
        Check: Future<Output = Result<TicketCommentPage, AppError>>,
    {
        let Some(load) = self.slot(Key {
            ticket,
            estimate_fields,
            limit,
            order,
        }) else {
            return fetch().await;
        };
        // Requests coalesced on the initial read share its live Jira authorization.
        let reused = load.initialized();
        let cached = load
            .get_or_try_init(|| async {
                let expires = Instant::now() + LIFETIME;
                let detail = fetch().await?;
                let bytes = retained_bytes(&detail);
                Ok::<_, AppError>(Cached {
                    detail,
                    expires,
                    bytes,
                })
            })
            .await?;
        self.trim();
        if reused {
            let comments = recheck(cached.detail.comments.issue).await?;
            Ok(TicketOpening {
                detail: cached.detail.detail.clone(),
                comments,
            })
        } else {
            Ok(cached.detail.clone())
        }
    }

    pub(in crate::jira) fn clear(&self) {
        self.entries().clear();
    }

    pub(in crate::jira) fn remove(&self, ticket: &TicketKey) {
        self.entries().retain(|entry| entry.key.ticket != *ticket);
    }

    fn entries(&self) -> MutexGuard<'_, VecDeque<Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn slot(&self, key: Key) -> Option<Arc<OnceCell<Cached>>> {
        let mut entries = self.entries();
        let now = Instant::now();
        entries.retain(|entry| match entry.load.get() {
            Some(value) => value.expires > now,
            None => Arc::strong_count(&entry.load) > 1,
        });
        if let Some(index) = entries.iter().position(|entry| entry.key == key) {
            let entry = entries.remove(index)?;
            let load = entry.load.clone();
            entries.push_back(entry);
            return Some(load);
        }
        if entries.len() == MAX_ENTRIES {
            let index = entries.iter().position(|entry| entry.load.initialized())?;
            entries.remove(index);
        }
        let load = Arc::new(OnceCell::new());
        entries.push_back(Entry {
            key,
            load: load.clone(),
        });
        Some(load)
    }

    fn trim(&self) {
        let mut entries = self.entries();
        let mut remaining = MAX_BYTES;
        for index in (0..entries.len()).rev() {
            if let Some(value) = entries[index].load.get() {
                if value.bytes <= remaining {
                    remaining -= value.bytes;
                } else {
                    entries.remove(index);
                }
            }
        }
    }
}

fn retained_bytes(opening: &TicketOpening) -> usize {
    let detail = &opening.detail;
    let comments = &opening.comments;
    size_of::<Cached>()
        + detail.identity.key.as_str().len()
        + detail.identity.title.capacity()
        + comments.url.capacity()
        + [
            &detail.status,
            &detail.priority,
            &detail.assignee,
            &detail.assignee_id,
            &detail.reporter,
            &detail.reporter_id,
            &detail.issue_type,
            &detail.description,
        ]
        .into_iter()
        .filter_map(Option::as_ref)
        .map(String::capacity)
        .sum::<usize>()
        + [&detail.labels, &detail.components]
            .into_iter()
            .map(|list| {
                list.capacity() * size_of::<String>()
                    + list.iter().map(String::capacity).sum::<usize>()
            })
            .sum::<usize>()
        + comments.items.capacity() * size_of::<TicketComment>()
        + comments
            .items
            .iter()
            .map(|comment| {
                comment.body.capacity()
                    + comment.author.as_ref().map_or(0, String::capacity)
                    + comment.author_id.as_ref().map_or(0, String::capacity)
            })
            .sum::<usize>()
}
