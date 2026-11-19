//! Private inbox occurrences and independent desktop delivery receipts.

mod desktop;
mod native;
pub use desktop::{DesktopClaim, DesktopNotifier, DesktopOutcome, DesktopStore, dispatch_desktop};
pub use native::{
    NativeAttention, NativeAttentionStore, NativeOccurrence, TrackedNative, poll_native_attention,
};

use reviewq_domain::identity::{NotificationId, ReviewId, Revision, RunId, SessionKey};

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;
use crate::labels::labels;

pub const MAX_INBOX_PAGE: u16 = 100;
pub const MAX_INBOX_SELECTION: usize = 2000;

labels!(Source {
    GitLab => "gitlab",
    Codex => "codex",
    Claude => "claude",
    Report => "report",
    Action => "action",
    Followup => "followup",
    Service => "service",
});
labels!(Kind {
    Reply => "reply",
    EditedReply => "edited_reply",
    Resolved => "resolved",
    Reopened => "reopened",
    NewHead => "new_head",
    ApprovalWithdrawn => "approval_withdrawn",
    AgentQuestion => "agent_question",
    AgentPermission => "agent_permission",
    AgentFailure => "agent_failure",
    Quota => "quota",
    ReportReady => "report_ready",
    ReportFailed => "report_failed",
    ActionNeedsInput => "action_needs_input",
    FollowupReady => "followup_ready",
    FollowupNeedsInput => "followup_needs_input",
});

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxView {
    Unread,
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationAction {
    Read,
    Unread,
    Dismiss,
}

pub struct Observation {
    pub source: Source,
    pub occurrence_id: String,
    pub kind: Kind,
    pub review: ReviewId,
    pub run: Option<RunId>,
    pub session: Option<SessionKey>,
    pub event_ref: Option<String>,
    pub label: String,
    pub occurred_at_ms: i64,
    pub desktop_episode: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboxPosition {
    pub scope: [u8; 32],
    pub limit: u16,
    pub view: InboxView,
    pub watermark: i64,
    pub revision: Revision,
    pub occurred_at_ms: i64,
    pub source: Source,
    pub id: NotificationId,
}

pub struct Notification {
    pub id: NotificationId,
    pub source: Source,
    pub kind: Kind,
    pub review: ReviewId,
    pub run: Option<RunId>,
    pub session: Option<SessionKey>,
    pub label: String,
    pub event_ref: Option<String>,
    pub occurred_at_ms: i64,
    pub read_at_ms: Option<i64>,
    pub notified_at_ms: Option<i64>,
    pub desktop_status: Option<String>,
    pub desktop_reason: Option<String>,
}

pub struct InboxPage {
    pub items: Vec<Notification>,
    pub next: Option<InboxPosition>,
    pub counts: UnreadCounts,
}

pub struct InboxChange {
    pub affected: Vec<NotificationId>,
    pub counts: UnreadCounts,
}

pub struct UnreadCounts {
    pub event_count: u64,
    pub mr_count: u64,
}

pub trait InboxStore: Send + Sync {
    fn read_inbox(
        &self,
        actor: Actor,
        view: InboxView,
        after: Option<InboxPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<InboxPage>>;
    fn assign_notifications(
        &self,
        actor: Actor,
        action: NotificationAction,
        ids: Vec<NotificationId>,
    ) -> AppFuture<'_, Snapshot<InboxChange>>;
}
