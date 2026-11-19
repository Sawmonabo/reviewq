use reviewq_domain::identity::{ReviewId, RunId};
use reviewq_domain::review_stage::RunStage;

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;

use super::{PageContext, QueueItem};

pub struct RunCardsFilter {
    pub stage: Option<RunStage>,
    pub reason: Option<String>,
    pub has_questions: Option<bool>,
    pub search: String,
}

pub struct RunCardsPosition {
    pub context: PageContext,
    pub activity: i64,
    pub review: ReviewId,
}

pub struct RunCard {
    pub review: QueueItem,
    pub runs: Vec<RunId>,
}

pub struct RunCards {
    pub items: Vec<RunCard>,
    pub next: Option<RunCardsPosition>,
    pub counts: StageCounts,
    pub total: u64,
}

#[derive(Default)]
pub struct StageCounts {
    pub needs_input: u64,
    pub working: u64,
    pub waiting: u64,
    pub archived: u64,
}

pub trait WorkspaceQueue: Send + Sync {
    fn run_cards(
        &self,
        actor: Actor,
        filter: RunCardsFilter,
        after: Option<RunCardsPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<RunCards>>;
}
