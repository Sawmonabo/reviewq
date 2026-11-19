//! Inbox occurrence and exact assignment API.

use serde::{Deserialize, Serialize};

use crate::{
    Page, ResourceIdDto, ReviewIdDto, RunIdDto, SafeCountDto, SessionKeyDto, UtcTimestampDto,
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StateResult {
    pub affected_ids: Vec<ResourceIdDto>,
    #[serde(flatten)]
    pub counts: UnreadCounts,
}
