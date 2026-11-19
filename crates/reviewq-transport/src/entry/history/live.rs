use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, stream};
use reviewq_app::access::AppError;
use reviewq_app::history::{
    HistorySession, NativeHistoryReader, NativeLiveSignal, NativeRpcStamp, SourceAnchor,
};
use reviewq_app::hosts::NativeHost;
use reviewq_domain::identity::{NativeInputId, NativeTurnId, RunId, SessionKey};
use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, OwnedSemaphorePermit, broadcast, watch};
use tokio::task::JoinHandle;

use crate::entry::error::ApiPath as Path;
use crate::entry::pages::parse;
use crate::entry::{ApiError, Entry};

type SourceKey = (RunId, SessionKey);

#[derive(Default)]
pub(in crate::entry) struct Watchers {
    sources: Mutex<HashMap<SourceKey, Weak<SharedWatch>>>,
    index_builds: Mutex<HashMap<SourceKey, Weak<AsyncMutex<()>>>>,
}

struct SharedWatch {
    source: HistorySession,
    updates: watch::Receiver<Option<Update>>,
    observer: JoinHandle<()>,
    fragments: broadcast::Sender<Arc<Fragment>>,
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
    epoch: u64,
}

struct Fragment {
    epoch: u64,
    turn: NativeTurnId,
    entry: NativeInputId,
    offset: u64,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Delta<'a> {
    run_id: &'a str,
    session_key: &'a str,
    generation: &'a str,
    turn_id: &'a str,
    entry_id: &'a str,
    block: u16,
    offset: u64,
    text: &'a str,
    provisional: bool,
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
    pub(super) async fn claim_overview(
        &self,
        session: &HistorySession,
    ) -> Result<OwnedMutexGuard<()>, AppError> {
        let key = (
            session.target.run_id().clone(),
            session.target.session_key().clone(),
        );
        let guard = {
            let mut builds = self.index_builds.lock().map_err(|_| AppError::Internal)?;
            builds.retain(|_, build| build.strong_count() > 0);
            if let Some(active) = builds.get(&key).and_then(Weak::upgrade) {
                active
            } else {
                let active = Arc::new(AsyncMutex::new(()));
                builds.insert(key, Arc::downgrade(&active));
                active
            }
        };
        Ok(guard.lock_owned().await)
    }

    fn open(
        &self,
        source: HistorySession,
        reader: Arc<dyn NativeHistoryReader>,
        host: Option<Arc<dyn NativeHost>>,
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
        let fragments = broadcast::channel(4).0;
        let observer = tokio::spawn(run(
            reader,
            host,
            source.clone(),
            notices,
            fragments.clone(),
        ));
        let result = Arc::new(SharedWatch {
            source,
            updates,
            observer,
            fragments,
        });
        active.insert(key, Arc::downgrade(&result));
        Ok(result)
    }
}

async fn run(
    reader: Arc<dyn NativeHistoryReader>,
    host: Option<Arc<dyn NativeHost>>,
    session: HistorySession,
    publish: watch::Sender<Option<Update>>,
    fragments: broadcast::Sender<Arc<Fragment>>,
) {
    let mut previous: Option<SourceAnchor> = None;
    let mut last_rpc: Option<NativeRpcStamp> = None;
    let mut last_availability = None;
    let mut sequence = 0_u64;
    let mut epoch = 0_u64;
    let mut reset_live = false;
    let mut live = None;
    let mut retry_live = tokio::time::Instant::now();
    let mut delay = Duration::from_secs(1);
    let mut events = None;
    loop {
        let check = reader.head(session.clone(), previous.clone()).await;
        let mut rpc = None;
        let (current, availability, reset) = match check {
            Ok(Some(stamp)) => (
                Some(stamp.anchor),
                "available",
                stamp.reset || last_rpc.is_some(),
            ),
            Ok(None)
                if session.binding.identity().provider()
                    == reviewq_domain::session::HostProvider::Codex
                    && host.is_some() =>
            {
                match host
                    .as_ref()
                    .expect("matched host")
                    .history_stamp(session.clone())
                    .await
                {
                    Ok(stamp) => {
                        let reset = last_rpc
                            .as_ref()
                            .is_some_and(|last| last.generation != stamp.generation)
                            || previous.is_some();
                        rpc = Some(stamp);
                        (None, "available", reset)
                    }
                    Err(AppError::NotFound) => (None, "missing", false),
                    Err(AppError::Forbidden | AppError::Unauthorized) => (None, "denied", false),
                    Err(AppError::Busy) => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                    Err(_) => (None, "disconnected", false),
                }
            }
            Err(AppError::Busy) => {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            Ok(None) | Err(AppError::NotFound) => (None, "missing", false),
            Err(AppError::Forbidden | AppError::Unauthorized) => (None, "denied", false),
            Err(AppError::InvalidRequest) => (None, "unsupported", false),
            Err(_) => (None, "disconnected", false),
        };
        let reset = reset || std::mem::take(&mut reset_live);
        let changed = current
            .as_ref()
            .is_some_and(|value| previous.as_ref() != Some(value))
            || rpc
                .as_ref()
                .is_some_and(|value| last_rpc.as_ref() != Some(value));
        let changed_availability = last_availability != Some(availability);
        if changed || changed_availability || reset {
            sequence = sequence.saturating_add(1);
            if reset || changed_availability {
                epoch = sequence;
            }
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
                .map(|a| a.generation.clone())
                .or_else(|| {
                    rpc.as_ref()
                        .or(last_rpc.as_ref())
                        .map(|stamp| stamp.generation.clone())
                });
            if publish
                .send(Some(Update {
                    sequence,
                    kind,
                    generation,
                    availability,
                    epoch,
                }))
                .is_err()
            {
                return;
            }
        }
        let file_available = current.is_some();
        if file_available {
            previous = current;
            last_rpc = None;
        } else if rpc.is_some() {
            previous = None;
            last_rpc = rpc;
            events = None;
        }
        last_availability = Some(availability);
        delay = if availability == "available" {
            Duration::from_secs(1)
        } else {
            (delay * 2).min(Duration::from_secs(30))
        };
        if file_available && events.is_none() {
            events = reader.watch(session.clone()).await.ok().flatten();
        }
        if availability == "available"
            && live.is_none()
            && let Some(host) = host.as_ref()
            && session.binding.identity().provider() == reviewq_domain::session::HostProvider::Codex
            && tokio::time::Instant::now() >= retry_live
        {
            // Keep the host connection read-only. Starting or resuming a thread is an effect.
            live = host.history_signals(session.clone()).await.ok().flatten();
            retry_live = tokio::time::Instant::now() + Duration::from_secs(10);
        }
        let poll = tokio::time::sleep(delay);
        tokio::pin!(poll);
        loop {
            tokio::select! {
                result = async {
                    match events.as_mut() {
                        Some(watch) => watch.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if result != Ok(true) { events = None; }
                    tokio::time::sleep(Duration::from_millis(75)).await;
                    break;
                }
                signal = async {
                    match live.as_mut() {
                        Some(watch) => watch.recv().await,
                        None => std::future::pending().await,
                    }
                } => match signal {
                    Ok(NativeLiveSignal::AssistantDelta { turn, entry, offset, text })
                        if availability == "available" => {
                            let _ = fragments.send(Arc::new(Fragment { epoch, turn, entry, offset, text }));
                    }
                    Ok(NativeLiveSignal::AssistantDelta { .. }) => {}
                    Ok(NativeLiveSignal::Completed) => break,
                    Ok(NativeLiveSignal::Reset) => {
                        reset_live = true;
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        live = live.as_ref().map(broadcast::Receiver::resubscribe);
                        reset_live = true;
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        live = None;
                        reset_live = true;
                        break;
                    }
                },
                () = &mut poll => break,
            }
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
    let lease =
        entry
            .history_watches
            .open(source, entry.file_history.clone(), entry.host.clone())?;
    let feed = Feed {
        entry: entry.clone(),
        updates: lease.updates.clone(),
        fragments: lease.fragments.subscribe(),
        pending_fragment: None,
        missed_fragments: false,
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
    fragments: broadcast::Receiver<Arc<Fragment>>,
    pending_fragment: Option<Arc<Fragment>>,
    missed_fragments: bool,
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
            if let Some(message) = message {
                let notice = Notice {
                    run_id: target.run_id().as_str(),
                    session_key: target.session_key().as_str(),
                    generation: message.generation.as_deref(),
                    availability: message.availability,
                };
                if self.missed_fragments || message.sequence > self.sequence {
                    let kind = if std::mem::take(&mut self.missed_fragments)
                        || (self.sequence != 0
                            && message.sequence > self.sequence.saturating_add(1))
                    {
                        "reset"
                    } else {
                        message.kind
                    };
                    self.sequence = message.sequence;
                    let event = Event::default().event(kind).json_data(&notice).ok()?;
                    return Some((Ok(event), self));
                }
                if let Some(fragment) = self.pending_fragment.take()
                    && message.availability == "available"
                    && fragment.epoch == message.epoch
                    && let Some(generation) = message.generation.as_deref()
                {
                    let delta = Delta {
                        run_id: notice.run_id,
                        session_key: notice.session_key,
                        generation,
                        turn_id: fragment.turn.as_str(),
                        entry_id: fragment.entry.as_str(),
                        block: 0,
                        offset: fragment.offset,
                        text: &fragment.text,
                        provisional: true,
                    };
                    let event = Event::default().event("delta").json_data(&delta).ok()?;
                    return Some((Ok(event), self));
                }
            }
            tokio::select! {
                result = self.updates.changed() => if result.is_err() { return None; },
                _ = self.shutdown.changed() => return None,
                result = self.fragments.recv() => match result {
                    Ok(fragment) => self.pending_fragment = Some(fragment),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        self.missed_fragments = true;
                        self.fragments = self.fragments.resubscribe();
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
                _ = tokio::time::sleep(Duration::from_secs(15)) => {},
            }
        }
    }
}
