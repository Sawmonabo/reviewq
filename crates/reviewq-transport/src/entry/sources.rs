use std::sync::Arc;

use reviewq_app::access::{Actor, AppError};
use reviewq_app::sources::ReviewSourceReader;
use reviewq_domain::identity::Revision;

use super::Entry;

#[derive(Clone)]
pub(super) struct ActiveSource {
    pub(super) revision: Revision,
    pub(super) reader: Option<Arc<dyn ReviewSourceReader>>,
    pub(super) collection_enabled: bool,
}

impl Entry {
    /// Existing readers retain their immutable credentials. A new operation reloads on revision change.
    pub(super) async fn active_source(&self) -> Result<ActiveSource, AppError> {
        let current = self.store.preference_revision().await?;
        let mut guard = self.source.lock().await;
        if let Some(cached) = &*guard
            && cached.revision >= current
        {
            return Ok(cached.clone());
        }
        let saved = self
            .store
            .read_credentials(Actor::local_operator())
            .await?
            .value;
        let (reader, collection_enabled) = match saved.gitlab {
            Some(gitlab) if gitlab.token.is_some() => {
                let both = saved.jira.is_some_and(|jira| jira.token.is_some());
                (Some(self.settings_sources.review_source(&gitlab)?), both)
            }
            Some(_) => (None, false),
            None => (
                self.configured_source.clone(),
                self.configured_source.is_some(),
            ),
        };
        let source = ActiveSource {
            revision: saved.revision,
            reader,
            collection_enabled,
        };
        *guard = Some(source.clone());
        Ok(source)
    }

    pub(super) async fn source_reader(&self) -> Result<Arc<dyn ReviewSourceReader>, AppError> {
        self.active_source()
            .await?
            .reader
            .ok_or(AppError::Unavailable)
    }
}
