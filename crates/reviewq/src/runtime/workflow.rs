use std::sync::Arc;
use std::time::Duration;

use reviewq_app::actions::run_due_publications;
use reviewq_app::followups::{poll_monitored_reviews, run_due_followups};
use reviewq_app::hosts::recovery::run_due_recoveries;
use reviewq_app::ledger::publish::settle_due;
use reviewq_app::notifications::poll_native_attention;
use reviewq_app::settings::RuntimeSources;
use reviewq_app::workflow::NativeWorkflowProof;
use reviewq_store::SqliteStore;
use tokio::sync::watch;

/// Every 10 seconds until shutdown: reads native attention, stores each Claude companion that
/// went silent as ended, settles the ledger runs that are due, which it also does once on
/// start, then polls the configured sources and runs the due follow-ups, recoveries and
/// publications. With `settle_only`, as in validation, each tick only ends silent companions
/// and settles, which read stored state alone and ask no provider. A step's error skips that
/// step for the tick, as a settle error does. Every step that reads whether a Claude session
/// is live or gone asks the store's one companion watch.
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
            _ = async {
                let _ = store.end_silent_companions().await;
                settle_due(store.as_ref()).await
            } => {},
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
