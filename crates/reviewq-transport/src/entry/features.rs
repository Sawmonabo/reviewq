//! Production HTTP routes for review-owned state.

pub(super) mod decisions;
mod notifications;
mod reports;

use std::sync::Arc;

use axum::routing::get;
use axum::{Router, extract::DefaultBodyLimit};

use super::Entry;

pub(super) fn routes() -> Router<Arc<Entry>> {
    Router::new()
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
