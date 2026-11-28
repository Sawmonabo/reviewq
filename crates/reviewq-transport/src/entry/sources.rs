use std::sync::Arc;
use std::time::Duration;

use reviewq_app::access::{AppError, Service};
use reviewq_app::settings::ActiveSource;
use reviewq_app::sources::ReviewSourceReader;

use super::Entry;

impl Entry {
    pub(super) async fn active_source(&self) -> Result<ActiveSource, AppError> {
        self.source.current().await
    }

    pub(super) async fn source_reader(&self) -> Result<Arc<dyn ReviewSourceReader>, AppError> {
        self.active_source()
            .await?
            .reader
            .ok_or(AppError::Unavailable)
    }

    /// The service that still asks ReviewQ to wait, and for how long, as [`paused`] reads it.
    pub(super) async fn source_pause(&self) -> Option<(Service, Duration)> {
        paused(&self.active_source().await.ok()?)
    }
}

/// The longer wait GitLab or Jira still asks of ReviewQ, with the service that asked, or
/// `None` when neither does.
pub(super) fn paused(source: &ActiveSource) -> Option<(Service, Duration)> {
    let gitlab = source.reader.as_ref().map(|reader| reader.rate_pause());
    let jira = source.tickets.as_ref().map(|tickets| tickets.rate_pause());
    [(Service::GitLab, gitlab), (Service::Jira, jira)]
        .into_iter()
        .filter_map(|(service, wait)| Some((service, wait?)))
        .filter(|(_, wait)| !wait.is_zero())
        .max_by_key(|(_, wait)| *wait)
}
