use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, stream};
use reviewq_app::access::AppError;
use reviewq_app::events::{Invalidation, Replay, ResetReason, ResourceKey};
use reviewq_domain::identity::EventCursor;
use reviewq_protocol::ResourceIdDto;
use reviewq_protocol::events::{
    InvalidateEvent, ResetEvent, ResetReasonDto, ResourceKeyDto, ResourceKindDto,
};
use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, watch};

use super::error::ApiQuery as Query;
use super::{ApiError, Entry};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Subscribe {
    after: Option<String>,
}

pub(super) async fn feed(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Query(query): Query<Subscribe>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    entry.auth.browser_actor(&headers, false)?;
    if headers.get_all("last-event-id").iter().count() > 1 {
        return Err(AppError::InvalidRequest.into());
    }
    let after = match headers.get("last-event-id") {
        Some(value) => Some(value.to_str().map_err(|_| AppError::InvalidRequest)?),
        None => query.after.as_deref(),
    };
    let updates = entry.commits.clone();
    let after = match after {
        Some(value) => value.parse().map_err(|_| AppError::InvalidRequest)?,
        None => *updates.borrow(),
    };
    let permit = entry
        .event_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::Busy)?;
    let feed = Feed {
        shutdown: entry.shutdown.clone(),
        entry,
        updates,
        after,
        pending: VecDeque::new(),
        finished: false,
        _permit: permit,
    };
    Ok(Sse::new(stream::unfold(feed, Feed::next))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

struct Feed {
    entry: Arc<Entry>,
    updates: watch::Receiver<EventCursor>,
    shutdown: watch::Receiver<bool>,
    after: EventCursor,
    pending: VecDeque<(EventCursor, Invalidation)>,
    finished: bool,
    _permit: OwnedSemaphorePermit,
}

impl Feed {
    async fn next(mut self) -> Option<(Result<Event, Infallible>, Self)> {
        loop {
            if self.finished || *self.shutdown.borrow() {
                return None;
            }
            if let Some((cursor, change)) = self.pending.pop_front() {
                self.after = cursor;
                let projected = change.keys().iter().map(key).collect::<Vec<_>>();
                let event = Event::default()
                    .id(cursor.to_string())
                    .event("invalidate")
                    .json_data(InvalidateEvent::new(projected)?)
                    .ok()?;
                return Some((Ok(event), self));
            }
            self.updates.borrow_and_update();
            match self.entry.store.replay(self.after, 64).await {
                Ok(Replay::Batch { events, .. }) if !events.is_empty() => {
                    self.pending.extend(events);
                }
                Ok(Replay::Batch { .. }) => {
                    tokio::select! {
                        updated = self.updates.changed() => if updated.is_err() { return None; },
                        _ = self.shutdown.changed() => return None,
                    }
                }
                replay => {
                    let reason = match replay {
                        Ok(Replay::Reset {
                            reason: ResetReason::CursorAhead,
                            ..
                        }) => ResetReasonDto::CursorAhead,
                        Ok(Replay::Reset {
                            reason: ResetReason::CursorExpired,
                            ..
                        }) => ResetReasonDto::CursorExpired,
                        _ => ResetReasonDto::StreamLag,
                    };
                    self.finished = true;
                    let reset = Event::default()
                        .event("reset")
                        .json_data(ResetEvent { reason })
                        .ok()?;
                    return Some((Ok(reset), self));
                }
            }
        }
    }
}

fn key(item: &ResourceKey) -> ResourceKeyDto {
    fn id<T: ToString>(value: &Option<T>) -> Option<ResourceIdDto> {
        value
            .as_ref()
            .and_then(|value| ResourceIdDto::new(value.to_string()))
    }
    let (kind, id) = match item {
        ResourceKey::Queue => (ResourceKindDto::Queue, None),
        ResourceKey::QueueSync => (ResourceKindDto::QueueSync, None),
        ResourceKey::Review(value) => (ResourceKindDto::Review, id(value)),
        ResourceKey::Run(value) => (ResourceKindDto::Run, id(value)),
        ResourceKey::Session(value) => (ResourceKindDto::Session, id(value)),
        ResourceKey::History(value) => (ResourceKindDto::History, id(value)),
        ResourceKey::Changes(value) => (ResourceKindDto::Changes, id(value)),
        ResourceKey::Discussions(value) => (ResourceKindDto::Discussions, id(value)),
        ResourceKey::Report(value) => (ResourceKindDto::Report, id(value)),
        ResourceKey::Action(value) => (ResourceKindDto::Action, id(value)),
        ResourceKey::Followup(value) => (ResourceKindDto::Followup, id(value)),
        ResourceKey::Ticket(value) => (ResourceKindDto::Ticket, id(value)),
        ResourceKey::Notification(value) => (ResourceKindDto::Notification, id(value)),
        ResourceKey::Settings => (ResourceKindDto::Settings, None),
    };
    ResourceKeyDto { kind, id }
}
