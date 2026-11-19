use std::sync::Arc;
use std::time::Duration;

use reviewq_app::actions::run_due_publications;
use reviewq_app::followups::{poll_monitored_reviews, run_due_followups};
use reviewq_app::settings::RuntimeSources;
use reviewq_app::workflow::NativeWorkflowProof;
use reviewq_store::SqliteStore;
use tokio::sync::watch;

pub(super) async fn run(
    store: Arc<SqliteStore>,
    sources: Arc<RuntimeSources>,
    native: Arc<dyn NativeWorkflowProof>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut cursor = None;
    let mut revision = None;
    while !*shutdown.borrow_and_update() {
        tokio::select! {
            _ = shutdown.changed() => break,
            () = tokio::time::sleep(Duration::from_secs(10)) => {},
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
                    run_due_publications(store.as_ref(), source.as_ref(), native.as_ref()),
                );
            } => {},
        }
    }
}
