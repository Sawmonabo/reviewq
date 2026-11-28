use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use reviewq_app::access::{Actor, ActorScope, AppError};
use reviewq_domain::identity::GrantId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// How many tool calls run at once for one checkout grant.
const PER_GRANT: usize = 8;
/// How many tool calls run at once overall.
const OVERALL: usize = 32;

/// The turns of MCP tool calls: a call waits, in arrival order, for a running place of its
/// checkout grant and then for one overall. The caller's tool deadline bounds the wait.
pub(super) struct Gates {
    overall: Arc<Semaphore>,
    grants: Mutex<HashMap<GrantId, Weak<Semaphore>>>,
}

pub(super) struct Permit {
    _overall: OwnedSemaphorePermit,
    _grant: OwnedSemaphorePermit,
}

impl Default for Gates {
    fn default() -> Self {
        Self {
            overall: Arc::new(Semaphore::new(OVERALL)),
            grants: Mutex::default(),
        }
    }
}

impl Gates {
    pub(super) async fn enter(&self, actor: &Actor) -> Result<Permit, AppError> {
        let ActorScope::CheckoutGrant(id) = actor.scope() else {
            return Err(AppError::Unauthorized);
        };
        let grant = {
            let mut grants = self.grants.lock().map_err(|_| AppError::Internal)?;
            grants.retain(|_, places| places.strong_count() > 0);
            match grants.get(id).and_then(Weak::upgrade) {
                Some(places) => places,
                None => {
                    let places = Arc::new(Semaphore::new(PER_GRANT));
                    grants.insert(id.clone(), Arc::downgrade(&places));
                    places
                }
            }
        };
        let grant = grant
            .acquire_owned()
            .await
            .map_err(|_| AppError::Unavailable)?;
        let overall = self
            .overall
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::Unavailable)?;
        Ok(Permit {
            _overall: overall,
            _grant: grant,
        })
    }
}
