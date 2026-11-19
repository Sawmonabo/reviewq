use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, stream};
use reviewq_app::access::AppError;
use reviewq_app::history::{HistorySession, NativeHistoryReader, SourceAnchor};
use reviewq_domain::identity::{RunId, SessionKey};
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, watch};
use tokio::task::JoinHandle;

use crate::entry::error::ApiPath as Path;
use crate::entry::pages::parse;
use crate::entry::{ApiError, Entry};

type SourceKey = (RunId, SessionKey);

#[derive(Default)]
pub(in crate::entry) struct Watchers {
    sources: Mutex<HashMap<SourceKey, Weak<SharedWatch>>>,
}

struct SharedWatch {
    source: HistorySession,
    updates: watch::Receiver<Option<Update>>,
    observer: JoinHandle<()>,
}

impl Drop for SharedWatch {
    fn drop(&mut self) {
        self.observer.abort();
    }
}

#[derive(Clone, Eq, PartialEq)]
struct Update {
    sequence: u64,
    kind: &'static str,
    generation: Option<String>,
    availability: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Notice<'a> {
    run_id: &'a str,
    session_key: &'a str,
    generation: Option<&'a str>,
    availability: &'static str,
}

impl Watchers {
    fn open(
        &self,
        source: HistorySession,
        reader: Arc<dyn NativeHistoryReader>,
    ) -> Result<Arc<SharedWatch>, AppError> {
        let key = (
            source.target.run_id().clone(),
            source.target.session_key().clone(),
        );
        let mut active = self.sources.lock().map_err(|_| AppError::Internal)?;
        active.retain(|_, watch| watch.strong_count() != 0);
        if let Some(watch) = active.get(&key).and_then(Weak::upgrade)
            && watch.source == source
        {
            return Ok(watch);
        }
        let (notices, updates) = watch::channel(None);
        let observer = tokio::spawn(run(reader, source.clone(), notices));
        let result = Arc::new(SharedWatch {
            source,
            updates,
            observer,
        });
        active.insert(key, Arc::downgrade(&result));
        Ok(result)
    }
}

async fn run(
    reader: Arc<dyn NativeHistoryReader>,
    session: HistorySession,
    publish: watch::Sender<Option<Update>>,
) {
    let mut previous: Option<SourceAnchor> = None;
    let mut last_availability = None;
    let mut sequence = 0_u64;
    let mut delay = Duration::from_secs(1);
    let mut events = None;
    loop {
        let check = reader.head(session.clone(), previous.clone()).await;
        let (current, availability, reset) = match check {
            Ok(Some(stamp)) => (Some(stamp.anchor), "available", stamp.reset),
            Err(AppError::Busy) => {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            Ok(None) | Err(AppError::NotFound) => (None, "missing", false),
            Err(AppError::Forbidden | AppError::Unauthorized) => (None, "denied", false),
            Err(AppError::InvalidRequest) => (None, "unsupported", false),
            Err(_) => (None, "disconnected", false),
        };
        let changed = current
            .as_ref()
            .is_some_and(|value| previous.as_ref() != Some(value));
        let changed_availability = last_availability != Some(availability);
        if changed || changed_availability || reset {
            sequence = sequence.saturating_add(1);
            let kind = if reset {
                "reset"
            } else if changed_availability {
                "availability"
            } else {
                "changed"
            };
            let generation = current
                .as_ref()
                .or(previous.as_ref())
                .map(|a| a.generation.clone());
            if publish
                .send(Some(Update {
                    sequence,
                    kind,
                    generation,
                    availability,
                }))
                .is_err()
            {
                return;
            }
        }
        if current.is_some() {
            previous = current;
        }
        last_availability = Some(availability);
        delay = if availability == "available" {
            Duration::from_secs(1)
        } else {
            (delay * 2).min(Duration::from_secs(30))
        };
        if availability == "available" && events.is_none() {
            events = reader.watch(session.clone()).await.ok().flatten();
        }
        if let Some(watch) = events.as_mut() {
            tokio::select! {
                result = watch.changed() => {
                    if result != Ok(true) { events = None; }
                    tokio::time::sleep(Duration::from_millis(75)).await;
                }
                () = tokio::time::sleep(delay) => {}
            }
        } else {
            tokio::time::sleep(delay).await;
        }
    }
}

pub(in crate::entry) async fn live(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let run: RunId = parse(&run)?;
    let session: SessionKey = parse(&session)?;
    let actor = entry.auth.browser_actor(&headers, false)?;
    let source = entry
        .store
        .history_session(actor, run, session)
        .await?
        .value;
    let permit = entry
        .event_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::Busy)?;
    let lease = entry
        .history_watches
        .open(source, entry.file_history.clone())?;
    let feed = Feed {
        entry: entry.clone(),
        updates: lease.updates.clone(),
        shutdown: entry.shutdown.clone(),
        headers,
        sequence: 0,
        finished: false,
        lease,
        _permit: permit,
    };
    Ok(Sse::new(stream::unfold(feed, Feed::next))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

struct Feed {
    entry: Arc<Entry>,
    updates: watch::Receiver<Option<Update>>,
    shutdown: watch::Receiver<bool>,
    lease: Arc<SharedWatch>,
    headers: HeaderMap,
    sequence: u64,
    _permit: OwnedSemaphorePermit,
    finished: bool,
}

impl Feed {
    async fn next(mut self) -> Option<(Result<Event, Infallible>, Self)> {
        loop {
            if self.finished || *self.shutdown.borrow() {
                return None;
            }
            let actor = self.entry.auth.browser_actor(&self.headers, false).ok()?;
            let target = &self.lease.source.target;
            let scoped = self
                .entry
                .store
                .history_session(actor, target.run_id().clone(), target.session_key().clone())
                .await
                .ok()?;
            if scoped.value != self.lease.source {
                self.finished = true;
                let notice = Notice {
                    run_id: target.run_id().as_str(),
                    session_key: target.session_key().as_str(),
                    generation: None,
                    availability: "disconnected",
                };
                let event = Event::default().event("reset").json_data(&notice).ok()?;
                return Some((Ok(event), self));
            }
            let message = self.updates.borrow_and_update().clone();
            if let Some(message) = message.filter(|value| value.sequence > self.sequence) {
                let kind =
                    if self.sequence != 0 && message.sequence > self.sequence.saturating_add(1) {
                        "reset"
                    } else {
                        message.kind
                    };
                self.sequence = message.sequence;
                let notice = Notice {
                    run_id: target.run_id().as_str(),
                    session_key: target.session_key().as_str(),
                    generation: message.generation.as_deref(),
                    availability: message.availability,
                };
                let event = Event::default().event(kind).json_data(&notice).ok()?;
                return Some((Ok(event), self));
            }
            tokio::select! {
                result=self.updates.changed()=>if result.is_err(){return None;},
                _=self.shutdown.changed()=>return None,
                _=tokio::time::sleep(Duration::from_secs(15))=>{},
            }
        }
    }
}
