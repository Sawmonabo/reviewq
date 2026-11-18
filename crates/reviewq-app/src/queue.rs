//! Verified review observations and scoped selection reads.

use reviewq_domain::identity::{GitSha, MrIdentity, ReviewId, RunId};
use reviewq_domain::session::LeadTarget;

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;

/// Current provider evidence used to create the compact registered review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSource {
    pub identity: MrIdentity,
    pub project_path: String,
    pub title: String,
    pub head_sha: GitSha,
    pub state: MergeRequestState,
    pub updated_at_ms: i64,
}

/// Authoritative provider lifecycle of a merge request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeRequestState {
    Open,
    Closed,
    Merged,
}

/// One scoped review and its requested or default registered lead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedLead {
    review_id: ReviewId,
    lead: Option<LeadTarget>,
}

impl SelectedLead {
    pub fn new(review_id: ReviewId, lead: Option<LeadTarget>) -> Result<Self, AppError> {
        if lead
            .as_ref()
            .is_some_and(|target| target.review_id() != &review_id)
        {
            return Err(AppError::NotFound);
        }
        Ok(Self { review_id, lead })
    }

    pub fn review_id(&self) -> &ReviewId {
        &self.review_id
    }

    pub fn lead(&self) -> Option<&LeadTarget> {
        self.lead.as_ref()
    }
}

/// Direct lookup used by review bookmarks and the selected-run browser path.
pub trait ReviewLookup: Send + Sync {
    /// An explicit missing or foreign run always returns scoped not-found.
    fn resolve_selection(
        &self,
        actor: Actor,
        review_id: ReviewId,
        run_id: Option<RunId>,
    ) -> AppFuture<'_, Snapshot<SelectedLead>>;
}
