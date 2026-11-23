//! Inbox occurrence and exact assignment API.

pub mod desktop;

use serde::{Deserialize, Serialize};

use crate::{
    MergeRequestIidDto, Page, ResourceIdDto, ReviewIdDto, RunIdDto, SafeCountDto, SessionKeyDto,
    TicketKeyDto, UtcTimestampDto,
};

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum InboxView {
    #[default]
    Unread,
    All,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Action {
    Read,
    Unread,
    Dismiss,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StateRequest {
    pub action: Action,
    pub ids: Vec<ResourceIdDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Tab {
    Changes,
    Agent,
    Findings,
    Overview,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Notification {
    pub id: ResourceIdDto,
    pub source: String,
    pub kind: String,
    pub review_id: ReviewIdDto,
    pub run_id: Option<RunIdDto>,
    pub session_key: Option<SessionKeyDto>,
    pub label: String,
    pub event_ref: Option<ResourceIdDto>,
    pub occurred_at: UtcTimestampDto,
    pub read_at: Option<UtcTimestampDto>,
    pub notified_at: Option<UtcTimestampDto>,
    pub desktop_status: Option<String>,
    pub desktop_reason: Option<String>,
    pub tab: Tab,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UnreadCounts {
    pub unread_event_count: SafeCountDto,
    pub unread_mr_count: SafeCountDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NotificationPage {
    #[serde(flatten)]
    pub page: Page<Notification>,
    #[serde(flatten)]
    pub counts: UnreadCounts,
    #[cfg_attr(feature = "schema", schemars(length(max = 100)))]
    pub review_contexts: Vec<ReviewContext>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NotificationDetail {
    pub item: Notification,
    pub review_context: ReviewContext,
    pub dismissed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReviewContext {
    pub review_id: ReviewIdDto,
    pub title: String,
    pub ticket_title: Option<String>,
    pub project_path: String,
    pub iid: MergeRequestIidDto,
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    pub ticket_keys: Vec<TicketKeyDto>,
    pub our_comments: SafeCountDto,
    pub unread_replies: SafeCountDto,
    pub personal_approval: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StateResult {
    pub affected_ids: Vec<ResourceIdDto>,
    #[serde(flatten)]
    pub counts: UnreadCounts,
}
