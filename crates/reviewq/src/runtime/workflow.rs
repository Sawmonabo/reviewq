use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use reviewq_app::actions::run_due_publications;
use reviewq_app::followups::{poll_monitored_reviews, run_due_followups};
use reviewq_app::hosts::recovery::run_due_recoveries;
use reviewq_app::ledger::SilentSessions;
use reviewq_app::ledger::publish::settle_due;
use reviewq_app::notifications::poll_native_attention;
use reviewq_app::settings::RuntimeSources;
use reviewq_app::workflow::NativeWorkflowProof;
use reviewq_store::SqliteStore;
use tokio::sync::watch;

/// How long ReviewQ watches without a break before a silent Claude session counts as ended:
/// the 30 seconds a session may stay silent, and one more of its 5-second reports.
const WATCH_BEFORE_SILENCE_COUNTS: Duration = Duration::from_secs(35);
/// How far the wall clock may move apart from elapsed time between two settle reads. More
/// means the machine slept or the clock was set, and stored report times say nothing yet.
const CLOCK_TOLERANCE: Duration = Duration::from_secs(5);

/// Since when ReviewQ has heard sessions report without a break. A session's silence is
/// measured on the wall clock, so a stopped service, a sleeping machine and a clock step all
/// look like 30 seconds of silence from a session that is still working.
struct Watch {
    since: Instant,
    read: (Instant, SystemTime),
}

impl Watch {
    fn start() -> Self {
        let now = Instant::now();
        Self {
            since: now,
            read: (now, SystemTime::now()),
        }
    }

    /// What a silent session counts as for the settle read about to run. Starts the watch
    /// again when the wall clock and elapsed time moved apart since the last read.
    fn silent_sessions(&mut self) -> SilentSessions {
        let now = (Instant::now(), SystemTime::now());
        let elapsed = now.0.duration_since(self.read.0);
        let apart = match now.1.duration_since(self.read.1) {
            Ok(wall) => wall.abs_diff(elapsed),
            Err(backwards) => backwards.duration() + elapsed,
        };
        if apart > CLOCK_TOLERANCE {
            self.since = now.0;
        }
        self.read = now;
        if now.0.duration_since(self.since) >= WATCH_BEFORE_SILENCE_COUNTS {
            SilentSessions::Ended
        } else {
            SilentSessions::Unknown
        }
    }
}

/// Every 10 seconds until shutdown: reads native attention, settles the ledger runs that are
/// due, which it also does once on start, then polls the configured sources and runs the due
/// follow-ups, recoveries and publications. With `settle_only`, as in validation, each tick
/// only settles, which reads stored state alone and asks no provider. A step's error skips
/// that step for the tick, as a settle error does. A settle read takes a silent Claude
/// session as ended only once `Watch` allows it.
pub(super) async fn run(
    store: Arc<SqliteStore>,
    sources: Arc<RuntimeSources>,
    native: Arc<dyn NativeWorkflowProof>,
    settle_only: bool,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut cursor = None;
    let mut revision = None;
    let mut native_cursor = None;
    let mut watch = Watch::start();
    if *shutdown.borrow_and_update() {
        return;
    }
    tokio::select! {
        _ = shutdown.changed() => return,
        _ = settle_due(store.as_ref(), watch.silent_sessions()) => {},
    }
    while !*shutdown.borrow_and_update() {
        tokio::select! {
            _ = shutdown.changed() => break,
            () = tokio::time::sleep(Duration::from_secs(10)) => {},
        }
        if !settle_only {
            let attention = tokio::select! {
                _ = shutdown.changed() => break,
                observed = poll_native_attention(store.as_ref(),native.as_ref(),native_cursor.clone()) => observed,
            };
            if let Ok(next) = attention {
                native_cursor = next;
            }
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = settle_due(store.as_ref(), watch.silent_sessions()) => {},
        }
        if settle_only {
            continue;
        }
        let Ok(context) = sources.current().await else {
            continue;
        };
        if revision != Some(context.revision) {
            revision = Some(context.revision);
            cursor = None;
        }
        let Some(source) = context.reader.filter(|_| context.collection_enabled) else {
            continue;
        };
        let observed = tokio::select! {
            _ = shutdown.changed() => break,
            observed = poll_monitored_reviews(store.as_ref(), source.as_ref(), cursor.clone()) => observed,
        };
        if let Ok(next) = observed {
            cursor = next;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = async {
                let _ = tokio::join!(
                    run_due_followups(store.as_ref(), source.as_ref(), native.as_ref()),
                    run_due_recoveries(store.as_ref(),source.as_ref(),native.as_ref(),context.revision),
                    run_due_publications(store.as_ref(), source.as_ref(), native.as_ref()),
                );
            } => {},
        }
    }
}
