//! Compact queue and review projections.

use serde::Serialize;

use crate::hosts::HostProviderDto;
use crate::scalar::{
    CheckpointIdDto, GitShaDto, MergeRequestIidDto, ProjectIdDto, ReviewIdDto, RevisionDto,
    RunIdDto, SafeCountDto, SessionKeyDto, TicketKeyDto, UpstreamIdDto,
};
use crate::{ErrorDto, EventCursorDto, Page, Success, UtcTimestampDto};

mod archive;
mod sprints;
mod tracking;
mod workspace;
pub use archive::{
    ArchiveBoard, ArchiveDate, ArchiveItem, ArchiveMembership, ArchivePage, ArchivePageResponse,
    parse_archive_bound,
};
pub use sprints::{LoadSprintsRequest, LoadSprintsResponse, LoadedSprints, SprintPair};
pub use tracking::{
    TicketComment, TicketDetail, TicketDetailResponse, TrackReviewRequest, TrackReviewResponse,
    TrackedReview,
};
pub use workspace::{RunCard, RunCards, RunCardsResponse, StageCounts};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct QueuePage {
    #[serde(flatten)]
    pub page: Page<QueueRow>,
    pub total_rows: SafeCountDto,
    pub total_mrs: SafeCountDto,
    pub facets: QueueFacets,
    pub sync: QueueSync,
    pub settings_revision: RevisionDto,
}

pub type QueuePageResponse = Success<QueuePage>;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum RefreshState {
    Running,
    Succeeded,
    Failed,
    Superseded,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RefreshReceipt {
    pub refresh_id: EventCursorDto,
    pub settings_revision: RevisionDto,
    pub state: RefreshState,
    pub finished_at: Option<UtcTimestampDto>,
    pub total_rows: Option<SafeCountDto>,
    pub error: Option<ErrorDto>,
}

pub type RefreshResponse = Success<RefreshReceipt>;

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum QueueRow {
    Mr(Box<MergeRequestRow>),
    UnmatchedTicket(UnmatchedTicket),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MergeRequestRow {
    pub review_id: ReviewIdDto,
    pub project_id: ProjectIdDto,
    pub project_path: String,
    pub iid: MergeRequestIidDto,
    pub title: String,
    pub tickets: Vec<CompactTicket>,
    pub author: Option<NamedUpstreamId>,
    pub source_sha: GitShaDto,
    pub mr_state: MergeRequestStatus,
    pub queue_state: QueueStatus,
    pub updated_at: UtcTimestampDto,
    pub draft: bool,
    pub own: bool,
    pub our_comments: SafeCountDto,
    pub unread_replies: SafeCountDto,
    pub personal_approval: Option<bool>,
    pub registered_lead_count: SafeCountDto,
    pub run_stage: Option<RunStage>,
    pub reason_kind: Option<String>,
    pub has_agent_questions: bool,
    pub focused_run_id: Option<RunIdDto>,
    pub focus_session_key: Option<SessionKeyDto>,
    pub focused_provider: Option<HostProviderDto>,
    pub focused_lead_name: Option<String>,
    pub focused_activity: Option<FocusedActivity>,
    pub registered_providers: Vec<HostProviderDto>,
    pub unread_non_reply_count: SafeCountDto,
    pub current_disposition: Option<crate::decisions::Disposition>,
    pub focused_launch: Option<FocusedLaunch>,
    pub focused_report_reviewed_sha: Option<GitShaDto>,
    pub full_mr_reviewed_sha: Option<GitShaDto>,
    pub coverage: Option<crate::reports::Coverage>,
    pub last_activity_at: Option<UtcTimestampDto>,
}

/// Most recent verified phase on the focused native session.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FocusedActivity {
    Idle,
    Working,
    Blocked,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FocusedLaunch {
    pub observed_model: Option<String>,
    pub observed_effort: Option<String>,
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CompactTicket {
    pub key: TicketKeyDto,
    pub title: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UnmatchedTicket {
    pub key: TicketKeyDto,
    pub title: String,
    pub status: String,
    pub priority: Option<String>,
    pub assignee: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct QueueFacets {
    pub boards: Vec<NamedUpstreamId>,
    pub projects: Vec<NamedUpstreamId>,
    pub sprints: Vec<SprintFacet>,
    pub authors: Vec<NamedUpstreamId>,
    pub hide_reviewed: SafeCountDto,
    pub hide_in_progress: SafeCountDto,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NamedUpstreamId {
    pub id: UpstreamIdDto,
    pub label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SprintFacet {
    pub board_id: UpstreamIdDto,
    pub id: UpstreamIdDto,
    pub label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct QueueSync {
    pub mode: QueueSyncMode,
    pub active: bool,
    pub stale: bool,
    pub last_attempt_at: Option<UtcTimestampDto>,
    pub last_success_at: Option<UtcTimestampDto>,
    pub error: Option<ErrorDto>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum QueueSyncMode {
    Setup,
    Connected,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MergeRequestStatus {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum QueueStatus {
    NeedsReview,
    WaitingAuthor,
    Followup,
    Reviewed,
    Archive,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum RunStage {
    NeedsInput,
    Working,
    Waiting,
    Archived,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReviewOverview {
    pub review: MergeRequestRow,
    pub web_url: String,
    pub decision_revision: RevisionDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<crate::decisions::DecisionView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention: Option<crate::decisions::Attention>,
    pub settings_revision: RevisionDto,
    pub checkpoint_id: Option<CheckpointIdDto>,
    pub runs: Page<RunSummary>,
}

pub type ReviewOverviewResponse = Success<ReviewOverview>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunSummary {
    pub run_id: RunIdDto,
    pub lead_session_key: SessionKeyDto,
    pub provider: HostProviderDto,
    pub lead_name: Option<String>,
    pub started_at: Option<UtcTimestampDto>,
}

#[derive(Debug, Serialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReviewSelection {
    WithLead {
        review_id: ReviewIdDto,
        run_id: RunIdDto,
        lead_session_key: SessionKeyDto,
    },
    WithoutLead {
        review_id: ReviewIdDto,
        run_id: (),
        lead_session_key: (),
    },
}

pub type ReviewSelectionResponse = Success<ReviewSelection>;
