use reviewq_domain::identity::{EventCursor, ReviewId, Revision, TicketKey};

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;
use crate::sources::TicketIdentity;

use super::ReviewSource;

pub struct TrackReview {
    pub settings: Revision,
    pub before: EventCursor,
    pub review: ReviewSource,
    pub tickets: Vec<TicketIdentity>,
}

pub struct TrackedReviewReceipt {
    pub id: ReviewId,
    pub review: ReviewSource,
}

#[derive(Eq, PartialEq)]
pub struct TicketAuthority {
    pub settings: Revision,
    pub estimate_fields: Vec<String>,
}

pub trait TrackingStore: Send + Sync {
    fn track(
        &self,
        actor: Actor,
        input: TrackReview,
    ) -> AppFuture<'_, Snapshot<TrackedReviewReceipt>>;
    fn ticket_authority(
        &self,
        actor: Actor,
        key: TicketKey,
    ) -> AppFuture<'_, Snapshot<TicketAuthority>>;
}
