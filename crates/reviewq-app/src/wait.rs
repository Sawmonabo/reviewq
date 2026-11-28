//! How a call waits for its turn. A call that finds every slot, queue place or lock taken
//! waits until one is free, in arrival order, instead of being refused: only a real limit
//! refuses a call. A caller with a deadline of its own, such as an MCP tool call, is cut by that
//! deadline first; `LONGEST` bounds every other wait, so nothing waits without a bound.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, SemaphorePermit};

use crate::access::AppError;

/// The longest any call waits for its turn: the longest deadline of a tool a model calls. The
/// MCP server checks at build time that its longest tool deadline is this value.
pub const LONGEST: Duration = Duration::from_secs(55);

/// Awaits `turn` for at most `LONGEST`, answering `Busy` past it.
pub async fn within<T>(turn: impl Future<Output = T>) -> Result<T, AppError> {
    tokio::time::timeout(LONGEST, turn)
        .await
        .map_err(|_| AppError::Busy)
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
