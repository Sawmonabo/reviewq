use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use reviewq_app::access::{Actor, ActorScope, AppError};
use reviewq_domain::identity::GrantId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) struct Gates {
    global: Arc<Gate>,
    grants: Mutex<HashMap<GrantId, Weak<Gate>>>,
}

struct Gate {
    active: Arc<Semaphore>,
    waiting: Semaphore,
}

pub(super) struct Permit {
    _global: OwnedSemaphorePermit,
    _grant: OwnedSemaphorePermit,
    _owner: Arc<Gate>,
}

impl Default for Gates {
    fn default() -> Self {
        Self {
            global: Arc::new(Gate::new(32, 64)),
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
            grants.retain(|_, owner| owner.strong_count() > 0);
            match grants.get(id).and_then(Weak::upgrade) {
                Some(grant) => grant,
                None => {
                    let grant = Arc::new(Gate::new(8, 8));
                    grants.insert(id.clone(), Arc::downgrade(&grant));
                    grant
                }
            }
        };
        if let Ok(grant_permit) = grant.active.clone().try_acquire_owned()
            && let Ok(global_permit) = self.global.active.clone().try_acquire_owned()
        {
            return Ok(Permit {
                _global: global_permit,
                _grant: grant_permit,
                _owner: grant.clone(),
            });
        }
        let _waiting = self
            .global
            .waiting
            .try_acquire()
            .map_err(|_| AppError::Busy)?;
        let _grant_waiting = grant.waiting.try_acquire().map_err(|_| AppError::Busy)?;
        let (grant_permit, global_permit) = tokio::time::timeout(Duration::from_secs(2), async {
            let grant = grant
                .active
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| AppError::Busy)?;
            let global = self
                .global
                .active
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| AppError::Busy)?;
            Ok::<_, AppError>((grant, global))
        })
        .await
        .map_err(|_| AppError::Busy)??;
        Ok(Permit {
            _global: global_permit,
            _grant: grant_permit,
            _owner: grant.clone(),
        })
    }
}

impl Gate {
    fn new(active: usize, waiting: usize) -> Self {
        Self {
            active: Arc::new(Semaphore::new(active)),
            waiting: Semaphore::new(waiting),
        }
    }
}
