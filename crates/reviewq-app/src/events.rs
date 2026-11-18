//! Atomic resource snapshots and the committed invalidation journal.

use reviewq_domain::identity::{
    EventCursor, NotificationId, ReviewId, RunId, SessionKey, TicketKey,
};

use crate::access::{AppError, AppFuture};

/// Maximum count of committed journal rows in one replay call.
pub const MAX_REPLAY_LIMIT: u16 = 128;
/// Maximum number of keys carried by one committed journal row.
pub const MAX_INVALIDATION_KEYS: usize = 32;

/// A projection and the event head observed in the same read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot<T> {
    pub value: T,
    pub cursor: EventCursor,
}

/// A resource or resource family to reload. No content belongs in a key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ResourceKey {
    Queue,
    Review(Option<ReviewId>),
    Run(Option<RunId>),
    Session(Option<SessionKey>),
    History(Option<SessionKey>),
    Changes(Option<ReviewId>),
    Discussions(Option<ReviewId>),
    Report(Option<RunId>),
    Action(Option<RunId>),
    Followup(Option<ReviewId>),
    Ticket(Option<TicketKey>),
    Notification(Option<NotificationId>),
    Settings,
}

/// The bounded invalidation keys of one committed journal row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Invalidation(Box<[ResourceKey]>);

impl Invalidation {
    pub fn new(keys: Vec<ResourceKey>) -> Result<Self, AppError> {
        if keys.is_empty() || keys.len() > MAX_INVALIDATION_KEYS {
            return Err(AppError::InvalidRequest);
        }
        Ok(Self(keys.into_boxed_slice()))
    }

    pub fn keys(&self) -> &[ResourceKey] {
        &self.0
    }
}

/// A terminal replay condition requiring authoritative reads before subscribing again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResetReason {
    CursorExpired,
    CursorAhead,
    StreamLag,
}

/// A bounded, ordered committed journal range or the need to restart the read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Replay {
    Batch {
        floor: EventCursor,
        head: EventCursor,
        /// Rows strictly after the requested cursor, in ascending cursor order.
        events: Vec<(EventCursor, Invalidation)>,
    },
    Reset {
        floor: EventCursor,
        head: EventCursor,
        reason: ResetReason,
    },
}

/// Reads durable replay independently of any live notification channel.
pub trait EventLog: Send + Sync {
    /// Reads up to `limit` committed rows after the given head in one snapshot.
    ///
    /// Rejects zero or a limit above `MAX_REPLAY_LIMIT`. Below-floor or
    /// above-head cursors return a reset with the same snapshot's bounds.
    fn replay(&self, after: EventCursor, limit: u16) -> AppFuture<'_, Replay>;
}
