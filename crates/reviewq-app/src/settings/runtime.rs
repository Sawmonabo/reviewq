use std::sync::Arc;

use reviewq_domain::identity::Revision;
use tokio::sync::Mutex;

use crate::access::{Actor, AppError};
use crate::sources::ReviewSourceReader;

use super::{ConnectionSources, SettingsStore};

#[derive(Clone)]
pub struct ActiveSource {
    pub revision: Revision,
    pub reader: Option<Arc<dyn ReviewSourceReader>>,
    pub collection_enabled: bool,
}

/// The revision of the actual verified credentials used for remote I/O.
#[derive(Clone)]
pub struct SourceLease {
    store: Arc<dyn SettingsStore>,
    revision: Revision,
}

impl SourceLease {
    pub async fn check(&self) -> Result<(), AppError> {
        if self.store.preference_revision().await? != self.revision {
            return Err(AppError::StaleRevision);
        }
        Ok(())
    }
}

pub struct RuntimeSources {
    store: Arc<dyn SettingsStore>,
    sources: Arc<dyn ConnectionSources>,
    validation: Option<Arc<dyn ReviewSourceReader>>,
    active: Mutex<Option<ActiveSource>>,
}

impl RuntimeSources {
    pub fn new(
        store: Arc<dyn SettingsStore>,
        sources: Arc<dyn ConnectionSources>,
        validation: Option<Arc<dyn ReviewSourceReader>>,
    ) -> Self {
        Self {
            store,
            sources,
            validation,
            active: Mutex::new(None),
        }
    }

    /// Refreshes the immutable client snapshot when the durable settings revision changes.
    pub async fn current(&self) -> Result<ActiveSource, AppError> {
        let current = self.store.preference_revision().await?;
        let mut guard = self.active.lock().await;
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
                let jira = saved.jira.is_some_and(|jira| jira.token.is_some());
                let lease = SourceLease {
                    store: self.store.clone(),
                    revision: saved.revision,
                };
                (Some(self.sources.review_source(&gitlab, lease)?), jira)
            }
            Some(_) => (None, false),
            None => (self.validation.clone(), self.validation.is_some()),
        };
        let active = ActiveSource {
            revision: saved.revision,
            reader,
            collection_enabled,
        };
        *guard = Some(active.clone());
        Ok(active)
    }
}
