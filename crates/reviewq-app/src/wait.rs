//! How a call waits for its turn. A call that finds every slot, queue place or lock taken
//! waits until one is free, in arrival order, instead of being refused: only a real limit
//! refuses a call. A caller with a deadline of its own, such as an MCP tool call, is cut by that
//! deadline first; `LONGEST` bounds every other wait, so nothing waits without a bound. A call
//! that waits more than once takes one `deadline` at its start and spends it across its waits,
//! so all of them together wait at most `LONGEST`.

use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore, SemaphorePermit, watch};
use tokio::time::Instant;

use crate::access::AppError;

/// The longest any call waits for its turn: the longest deadline of a tool a model calls. The
/// MCP server checks at build time that its longest tool deadline is this value.
pub const LONGEST: Duration = Duration::from_secs(55);

/// How often a wait on a blocking thread looks at its caller's stop flag.
const STOP_CHECK: Duration = Duration::from_millis(50);

/// The deadline of a call that starts now: `LONGEST` from now.
pub fn deadline() -> Instant {
    Instant::now() + LONGEST
}

/// Awaits `turn` for at most `LONGEST`, answering `Busy` past it.
pub async fn within<T>(turn: impl Future<Output = T>) -> Result<T, AppError> {
    until(deadline(), turn).await
}

/// Awaits `turn` until `deadline`, answering `Busy` past it.
pub async fn until<T>(deadline: Instant, turn: impl Future<Output = T>) -> Result<T, AppError> {
    tokio::time::timeout_at(deadline, turn)
        .await
        .map_err(|_| AppError::Busy)
}

/// Awaits `turn` until `deadline` from a blocking thread of the runtime, and leaves with
/// `Unavailable` once `stop` is set, so a caller that went away frees what it holds.
pub fn blocking<T>(
    turn: impl Future<Output = T>,
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<T, AppError> {
    let runtime = Handle::try_current().map_err(|_| AppError::Internal)?;
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
    owned_until(slots, deadline()).await
}

/// Waits for one of `slots` until `deadline`, as a permit that owns its semaphore.
pub async fn owned_until(
    slots: Arc<Semaphore>,
    deadline: Instant,
) -> Result<OwnedSemaphorePermit, AppError> {
    until(deadline, slots.acquire_owned())
        .await?
        .map_err(|_| AppError::Unavailable)
}

/// One of a cache's places, shared by every caller using one entry. The entry keeps a
/// `Weak` copy, so a caller of an entry in use joins its place, and the place is free again
/// when the entry's last caller lets go of it. A cache that bounds its entries in use by its
/// places therefore holds a place per entry, never per caller.
pub type Place = Arc<OwnedSemaphorePermit>;

/// The places of one cache, with word to the callers waiting for one whenever a caller takes
/// a place for an entry, so a waiter joins an entry another caller made meanwhile instead of
/// waiting for a place of its own.
pub struct Places {
    slots: Arc<Semaphore>,
    taken: watch::Sender<()>,
}

impl Places {
    pub fn new(count: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(count)),
            taken: watch::channel(()).0,
        }
    }

    /// A caller's turn for a place, waiting until `deadline` at the latest.
    pub fn turn(&self, deadline: Instant) -> Turn<'_> {
        Turn {
            places: self,
            seen: self.taken.subscribe(),
            held: None,
            queued: None,
            deadline,
        }
    }

    /// The places no entry holds.
    pub fn free(&self) -> usize {
        self.slots.available_permits()
    }
}

type Queued = Pin<Box<dyn Future<Output = Result<OwnedSemaphorePermit, AcquireError>> + Send>>;

/// One caller's turn for a place. The caller looks its entry up under the cache's own lock
/// and calls `take` only when the entry needs a place no one holds; when `take` has none, it
/// lets go of the lock, `wait`s, and looks again. The turn is marked as having seen every
/// place taken before it began and before each `wait` returns, so a place another caller takes
/// after a look always wakes the next `wait`.
pub struct Turn<'a> {
    places: &'a Places,
    seen: watch::Receiver<()>,
    held: Option<OwnedSemaphorePermit>,
    /// The caller's place in line, kept across looks so it never loses its turn.
    queued: Option<Queued>,
    deadline: Instant,
}

impl Turn<'_> {
    /// The place this turn was given, or one that is free now, behind no earlier waiter. A
    /// caller takes one only to make an entry or restart one, under the cache's lock, so the
    /// waiters it wakes find that entry when they get the lock.
    pub fn take(&mut self) -> Option<Place> {
        let permit = self
            .held
            .take()
            .or_else(|| self.places.slots.clone().try_acquire_owned().ok())?;
        self.places.taken.send_replace(());
        Some(Arc::new(permit))
    }

    /// Waits until this turn is given a place or another caller takes one, whichever is
    /// first, keeping its place in line; `Busy` once the deadline passes.
    pub async fn wait(&mut self) -> Result<(), AppError> {
        until(self.deadline, self.next()).await?
    }

    /// Waits like `wait`, from a blocking thread, leaving once `stop` is set.
    pub fn wait_blocking(&mut self, stop: &AtomicBool) -> Result<(), AppError> {
        let deadline = self.deadline;
        blocking(self.next(), deadline, stop)?
    }

    async fn next(&mut self) -> Result<(), AppError> {
        let Self {
            places,
            seen,
            held,
            queued,
            ..
        } = self;
        let line = queued.get_or_insert_with(|| Box::pin(places.slots.clone().acquire_owned()));
        let mut taken = pin!(seen.changed());
        let given = poll_fn(|context| {
            if let Poll::Ready(permit) = line.as_mut().poll(context) {
                return Poll::Ready(Some(permit));
            }
            // Its sender lives as long as the places, so only a place taken wakes it.
            taken.as_mut().poll(context).map(|_| None)
        })
        .await;
        if let Some(permit) = given {
            *queued = None;
            *held = Some(permit.map_err(|_| AppError::Unavailable)?);
        }
        Ok(())
    }
}
