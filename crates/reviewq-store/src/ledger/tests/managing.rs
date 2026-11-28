//! What `publish_is_conditional`'s managing reads return while the lead holds draft records:
//! the findings read of the current version and the review search by finding title.

use reviewq_app::access::Actor;
use reviewq_app::queue::{ManagementFilter, ReviewLookup};
use reviewq_app::reports::{
    Collection, ContextRequest, Descriptor, FindingSelection, FindingState, ReportStore,
};

use crate::SqliteStore;

/// Each finding of the current version as the managing findings read returns it: ID, title
/// and whether it has an outcome.
pub(super) async fn published_findings(store: &SqliteStore) -> Vec<(String, String, bool)> {
    let findings = FindingSelection {
        state: FindingState::All,
        kind: None,
        full_text: true,
    };
    let request = ContextRequest {
        receipt: None,
        collection: Collection::Findings,
        findings,
        after: None,
        limit: 10,
    };
    let context = store
        .read_report_context(Actor::local_operator(), "run".parse().unwrap(), request)
        .await
        .unwrap();
    context
        .value
        .items
        .into_iter()
        .map(|item| match item {
            Descriptor::Finding {
                id, title, outcome, ..
            } => (id, title, outcome.is_some()),
            _ => panic!("a findings read returns findings"),
        })
        .collect()
}

/// The finding a managing search for `query` names on the fixture review, which it lists
/// only when the review matched.
pub(super) async fn found_by_title(store: &SqliteStore, query: &str) -> Option<String> {
    store
        .write(|tx| {
            tx.sql.execute(
                "INSERT OR IGNORE INTO review_discoveries VALUES('review')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let page = store
        .management_reviews(
            Actor::local_operator(),
            ManagementFilter::All,
            query.into(),
            0,
            10,
        )
        .await
        .unwrap()
        .value;
    assert_eq!(page.total, page.findings.len() as u64);
    page.findings
        .into_iter()
        .next()
        .map(|found| found.finding_id)
}
