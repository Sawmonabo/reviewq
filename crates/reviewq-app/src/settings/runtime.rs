use std::sync::Arc;

use reviewq_domain::identity::Revision;
use reviewq_domain::settings::WorkspaceScope;
use tokio::sync::Mutex;

use crate::access::{Actor, AppError};
use crate::sources::{ReviewSourceReader, TicketSourceReader};

use super::{ConnectionSources, SettingsStore};

#[derive(Clone)]
pub struct ActiveSource {
    pub revision: Revision,
    pub reader: Option<Arc<dyn ReviewSourceReader>>,
    pub tickets: Option<Arc<dyn TicketSourceReader>>,
    pub scope: Arc<WorkspaceScope>,
    pub collection_enabled: bool,
    pub configured: bool,
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
        let lease = SourceLease {
            store: self.store.clone(),
            revision: saved.revision,
        };
        let configured =
            saved.gitlab.is_some() || saved.jira.is_some() || self.validation.is_some();
        let tickets = match saved.jira {
            Some(jira) if jira.token.is_some() => {
                Some(self.sources.ticket_source(&jira, lease.clone())?)
            }
            _ => None,
        };
        let (reader, collection_enabled) = match saved.gitlab {
            Some(gitlab) if gitlab.token.is_some() => (
                Some(self.sources.review_source(&gitlab, lease)?),
                tickets.is_some(),
            ),
            Some(_) => (None, false),
            None => (self.validation.clone(), self.validation.is_some()),
        };
        let active = ActiveSource {
            revision: saved.revision,
            reader,
            tickets,
            scope: Arc::new(saved.scope),
            collection_enabled,
            configured,
        };
        *guard = Some(active.clone());
        Ok(active)
    }
}
