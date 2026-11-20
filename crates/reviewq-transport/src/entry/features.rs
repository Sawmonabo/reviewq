//! Production HTTP routes for review-owned state.

pub(super) mod actions;
pub(super) mod decisions;
pub(super) mod followups;
pub(super) mod notifications;
pub(super) mod recovery;
pub(super) mod reports;

use std::sync::Arc;

use axum::routing::get;
use axum::{Router, extract::DefaultBodyLimit};

use super::Entry;

pub(super) fn routes() -> Router<Arc<Entry>> {
    Router::new()
        .merge(recovery::routes())
        .route(
            "/followups/preferences",
            get(followups::preferences).put(followups::save_preferences),
        )
        .route(
            "/reviews/{review_id}/followups/configure",
            axum::routing::post(followups::configure).layer(DefaultBodyLimit::max(512 * 1024)),
        )
        .route(
            "/reviews/{review_id}/followups/pause",
            axum::routing::post(followups::pause),
        )
        .route(
            "/reviews/{review_id}/followups/resume",
            axum::routing::post(followups::resume),
        )
        .route(
            "/reviews/{review_id}/followups/review-now",
            axum::routing::post(followups::review_now),
        )
        .route("/reviews/{review_id}/followups", get(followups::enrollment))
        .route(
            "/reviews/{review_id}/followups/results",
            get(followups::results),
        )
        .route(
            "/reviews/{review_id}/followups/assessments",
            get(followups::assessments),
        )
        .route("/runs/{run_id}/action", get(actions::current))
        .route(
            "/native/actions/declare",
            axum::routing::post(actions::declare).layer(DefaultBodyLimit::max(80 * 1024)),
        )
        .route(
            "/native/actions/reconcile",
            axum::routing::post(actions::reconcile),
        )
        .route(
            "/reviews/{review_id}/decisions",
            axum::routing::post(decisions::save),
        )
        .route(
            "/reviews/{review_id}/checkpoint",
            axum::routing::post(decisions::checkpoint),
        )
        .route("/notifications", get(notifications::list))
        .route(
            "/notifications/state",
            axum::routing::post(notifications::update).layer(DefaultBodyLimit::max(2_100_000)),
        )
        .route(
            "/runs/{run_id}/report/dispositions",
            axum::routing::put(reports::set_disposition),
        )
        .route(
            "/runs/{run_id}/report/viewed",
            axum::routing::post(reports::viewed),
        )
        .route(
            "/runs/{run_id}/report/feedback",
            axum::routing::post(reports::feedback).layer(DefaultBodyLimit::max(160 * 1024)),
        )
        .route(
            "/native/reports/accepted",
            axum::routing::post(reports::accepted),
        )
        .route(
            "/native/reports/handoff",
            axum::routing::post(reports::handoff).layer(DefaultBodyLimit::max(144 * 1024)),
        )
        .route(
            "/native/reports/handoff-status",
            axum::routing::post(reports::handoff_status).layer(DefaultBodyLimit::max(144 * 1024)),
        )
        .route("/runs/{run_id}/report", get(reports::current))
        .route("/runs/{run_id}/report/history", get(reports::history))
        .route("/runs/{run_id}/report/context", get(reports::context))
        .route("/runs/{run_id}/report/section", get(reports::section))
        .route(
            "/runs/{run_id}/report/versions/{receipt}",
            get(reports::version),
        )
        .route(
            "/runs/{run_id}/report/versions/{receipt}/window",
            get(reports::read_window),
        )
        .route(
            "/runs/{run_id}/report/versions/{receipt}/quote/resolve",
            axum::routing::post(reports::resolve_quote).layer(DefaultBodyLimit::max(160 * 1024)),
        )
        .route(
            "/runs/{run_id}/report/versions/{receipt}/source",
            get(reports::source),
        )
        .route(
            "/runs/{run_id}/report/candidates/{receipt}",
            get(reports::candidate),
        )
}
