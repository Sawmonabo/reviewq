//! Durable SSE event bodies. The cursor is sent separately as the SSE id.

use serde::Serialize;

use crate::scalar::ResourceIdDto;

/// Bounded resource descriptors for an `invalidate` event.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InvalidateEvent {
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 32)))]
    resources: Vec<ResourceKeyDto>,
}

impl InvalidateEvent {
    pub fn new(resources: Vec<ResourceKeyDto>) -> Option<Self> {
        (1..=32)
            .contains(&resources.len())
            .then_some(Self { resources })
    }
}

/// Reload one resource or the whole family when its ID is omitted.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ResourceKeyDto {
    pub kind: ResourceKindDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "ResourceIdDto"))]
    pub id: Option<ResourceIdDto>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ResourceKindDto {
    Queue,
    QueueSync,
    Review,
    Run,
    Session,
    History,
    Changes,
    Discussions,
    Report,
    Action,
    Followup,
    Ticket,
    Notification,
    Settings,
}

/// The terminal `reset` event body. A reset has no SSE id.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ResetEvent {
    pub reason: ResetReasonDto,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ResetReasonDto {
    CursorExpired,
    CursorAhead,
    StreamLag,
}
