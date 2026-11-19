//! Production HTTP routes for review-owned state.

mod actions;
pub(super) mod decisions;
mod followups;
mod notifications;
mod reports;

use std::sync::Arc;

use axum::routing::get;
use axum::{Router, extract::DefaultBodyLimit};

use super::Entry;

pub(super) fn routes() -> Router<Arc<Entry>> {
    Router::new()
        .route("/followups/preferences", get(followups::preferences))
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
        .route("/runs/{run_id}/report", get(reports::current))
        .route("/runs/{run_id}/report/history", get(reports::history))
        .route("/runs/{run_id}/report/context", get(reports::context))
        .route("/runs/{run_id}/report/section", get(reports::section))
        .route(
            "/runs/{run_id}/report/versions/{receipt}",
            get(reports::version),
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
