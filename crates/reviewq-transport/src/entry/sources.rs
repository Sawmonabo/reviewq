use std::sync::Arc;
use std::time::Duration;

use reviewq_app::access::AppError;
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

    /// The longer wait GitLab or Jira still asks of ReviewQ, or `None` when neither does.
    pub(super) async fn source_pause(&self) -> Option<Duration> {
        let source = self.active_source().await.ok()?;
        let gitlab = source.reader.as_ref().map(|reader| reader.rate_pause());
        let jira = source.tickets.as_ref().map(|tickets| tickets.rate_pause());
        gitlab
            .into_iter()
            .chain(jira)
            .max()
            .filter(|wait| !wait.is_zero())
    }
}
