use std::sync::Arc;

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
}
