//! How a call waits for its turn. A call that finds every slot, queue place or lock taken
//! waits until one is free, in arrival order, instead of being refused: only a real limit
//! refuses a call. A caller with a deadline of its own, such as an MCP tool call, is cut by that
//! deadline first; `LONGEST` bounds every other wait, so nothing waits without a bound.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, SemaphorePermit};
use tokio::time::Instant;

use crate::access::AppError;

/// The longest any call waits for its turn: the longest deadline of a tool a model calls. The
/// MCP server checks at build time that its longest tool deadline is this value.
pub const LONGEST: Duration = Duration::from_secs(55);

/// How often a wait on a blocking thread looks at its caller's stop flag.
const STOP_CHECK: Duration = Duration::from_millis(50);

/// Awaits `turn` for at most `LONGEST`, answering `Busy` past it.
pub async fn within<T>(turn: impl Future<Output = T>) -> Result<T, AppError> {
    tokio::time::timeout(LONGEST, turn)
        .await
        .map_err(|_| AppError::Busy)
}

/// Awaits `turn` like `within`, from a blocking thread of the runtime, and leaves with
/// `Unavailable` once `stop` is set, so a caller that went away frees what it holds.
pub fn blocking<T>(turn: impl Future<Output = T>, stop: &AtomicBool) -> Result<T, AppError> {
    let runtime = Handle::try_current().map_err(|_| AppError::Internal)?;
    let deadline = Instant::now() + LONGEST;
    // The same future is polled throughout, so the call keeps its place in the queue.
    let mut turn = pin!(turn);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(AppError::Unavailable);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(AppError::Busy);
        }
        if let Ok(value) =
            runtime.block_on(tokio::time::timeout(left.min(STOP_CHECK), turn.as_mut()))
        {
            return Ok(value);
        }
    }
}

/// Waits for one of `slots`. A closed semaphore answers `Unavailable`.
pub async fn permit(slots: &Semaphore) -> Result<SemaphorePermit<'_>, AppError> {
    within(slots.acquire())
        .await?
        .map_err(|_| AppError::Unavailable)
}

/// Waits for one of `slots`, as a permit that owns its semaphore.
pub async fn owned(slots: Arc<Semaphore>) -> Result<OwnedSemaphorePermit, AppError> {
    within(slots.acquire_owned())
        .await?
        .map_err(|_| AppError::Unavailable)
}

/// One of a cache's places, shared by every caller using one entry. The entry keeps a
/// `Weak` copy, so a caller of an entry in use joins its place, and the place is free again
/// when the entry's last caller lets go of it. A cache that bounds its entries in use by its
/// places therefore holds a place per entry, never per caller.
pub type Place = Arc<OwnedSemaphorePermit>;

/// One caller's turn for a new place among `places`. The caller looks its entry up under the
/// cache's own lock and calls `take` only when the entry needs a place no one holds; when
/// `take` has none, it lets go of the lock, `wait`s, and looks again, because another caller
/// may have made the entry meanwhile.
pub struct Turn {
    places: Arc<Semaphore>,
    held: Option<OwnedSemaphorePermit>,
}

impl Turn {
    pub fn new(places: &Arc<Semaphore>) -> Self {
        Self {
            places: places.clone(),
            held: None,
        }
    }

    /// The place this turn waited for, or one that is free now, behind no earlier waiter.
    pub fn take(&mut self) -> Option<Place> {
        self.held
            .take()
            .or_else(|| self.places.clone().try_acquire_owned().ok())
            .map(Arc::new)
    }

    /// Waits for a place, in arrival order, for at most `LONGEST`.
    pub async fn wait(&mut self) -> Result<(), AppError> {
        self.held = Some(owned(self.places.clone()).await?);
        Ok(())
    }

    /// Waits for a place like `wait`, from a blocking thread, leaving once `stop` is set.
    pub fn wait_blocking(&mut self, stop: &AtomicBool) -> Result<(), AppError> {
        let permit = blocking(self.places.clone().acquire_owned(), stop)?;
        self.held = Some(permit.map_err(|_| AppError::Unavailable)?);
        Ok(())
    }
}
