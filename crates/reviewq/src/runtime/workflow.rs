use std::sync::Arc;
use std::time::Duration;

use reviewq_app::actions::run_due_publications;
use reviewq_app::followups::{poll_monitored_reviews, run_due_followups};
use reviewq_app::hosts::recovery::run_due_recoveries;
use reviewq_app::ledger::publish::settle_due;
use reviewq_app::notifications::poll_native_attention;
use reviewq_app::settings::RuntimeSources;
use reviewq_app::workflow::NativeWorkflowProof;
use reviewq_store::{SqliteStore, WATCH_TICK};
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

/// Until shutdown, two ticks. First, and then every `WATCH_TICK` in a task of its own, so a
/// slow or blocking step of the other tick never delays it, the store reads its companion
/// watch and stores each Claude companion that went silent, or whose host process is proven
/// dead, as ended; so a session that died while ReviewQ was stopped is gone before the first
/// settle. Every 10 seconds: reads native attention, settles the ledger runs that are due,
/// which it also does once on start, then polls the configured sources and runs the due
/// follow-ups, recoveries and publications. With `settle_only`, as in validation, the second
/// tick only settles, which reads stored state alone and asks no provider. A step's error
/// skips that step for the tick, as a settle error does. Every step that reads whether a
/// Claude session is live or gone asks the store's one companion watch.
pub(super) async fn run(
    store: Arc<SqliteStore>,
    sources: Arc<RuntimeSources>,
    native: Arc<dyn NativeWorkflowProof>,
    settle_only: bool,
    shutdown: watch::Receiver<bool>,
) {
    let _ = store.end_companions().await;
    // Dropping the set aborts the watch, so it ends with this task.
    let mut watch = tokio::task::JoinSet::new();
    watch.spawn(watch_companions(Arc::clone(&store)));
    work(store, sources, native, settle_only, shutdown).await;
}

/// Ends silent and dead companions every `WATCH_TICK`, which also reads the watch each time.
async fn watch_companions(store: Arc<SqliteStore>) {
    let first = tokio::time::Instant::now() + WATCH_TICK;
    let mut tick = tokio::time::interval_at(first, WATCH_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let _ = store.end_companions().await;
    }
}

async fn work(
    store: Arc<SqliteStore>,
    sources: Arc<RuntimeSources>,
    native: Arc<dyn NativeWorkflowProof>,
    settle_only: bool,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut cursor = None;
    let mut revision = None;
    let mut native_cursor = None;
    if *shutdown.borrow_and_update() {
        return;
    }
    tokio::select! {
        _ = shutdown.changed() => return,
        _ = settle_due(store.as_ref()) => {},
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
            _ = settle_due(store.as_ref()) => {},
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
