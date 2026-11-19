use serde::Serialize;

use crate::scalar::{EventCursorDto, PageCursorDto};

/// An RFC 3339 UTC timestamp with exact millisecond precision and a trailing Z.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UtcTimestampDto(
    #[cfg_attr(feature = "schema", schemars(schema_with = "timestamp_schema"))] Box<str>,
);

/// The resource and event head from the same authorized snapshot.
#[derive(Debug, serde::Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Success<T> {
    pub data: T,
    pub cursor: EventCursorDto,
}

/// A bounded compact list with a required nullable keyset continuation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(rename = "{T}Page"))]
pub struct Page<T> {
    #[cfg_attr(feature = "schema", schemars(length(max = 100)))]
    items: Vec<T>,
    next_cursor: Option<PageCursorDto>,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, next_cursor: Option<PageCursorDto>) -> Option<Self> {
        (items.len() <= 100).then_some(Self { items, next_cursor })
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }
}

/// The error envelope for a rejected JSON operation.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Failure {
    pub error: ErrorDto,
    pub cursor: Option<EventCursorDto>,
}

/// A safe error projection suitable for the requesting client.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ErrorDto {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

/// Stable error codes for the public JSON envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ErrorCode {
    TicketUnverified,
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    StaleRevision,
    IdempotencyConflict,
    CursorExpired,
    PreconditionFailed,
    PayloadTooLarge,
    RangeNotSatisfiable,
    Busy,
    Unavailable,
    Internal,
}

impl UtcTimestampDto {
    pub fn from_unix_milliseconds(value: i64) -> Option<Self> {
        let timestamp = jiff::Timestamp::from_millisecond(value).ok()?;
        Self::new(&format!("{timestamp:.3}"))
    }

    /// Accepts a valid calendar date written as YYYY-MM-DDTHH:MM:SS.sssZ.
    pub fn new(value: &str) -> Option<Self> {
        if value.len() != 24 {
            return None;
        }
        let date: jiff::civil::DateTime = value.strip_suffix('Z')?.parse().ok()?;
        (format!("{date:.3}Z") == value).then(|| Self(value.into()))
    }
}

#[cfg(feature = "schema")]
fn timestamp_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "format": "date-time",
        "pattern": r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}Z$",
    })
}
