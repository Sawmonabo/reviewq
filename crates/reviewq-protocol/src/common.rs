use serde::{Serialize, Serializer};

const MAX_EVENT_CURSOR: u64 = i64::MAX as u64;

/// A durable event head, serialized as a canonical decimal string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EventCursorDto(
    #[cfg_attr(feature = "schema", schemars(schema_with = "event_cursor_schema"))] u64,
);

/// An RFC 3339 UTC timestamp with exact millisecond precision and a trailing Z.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UtcTimestampDto(
    #[cfg_attr(feature = "schema", schemars(schema_with = "timestamp_schema"))] Box<str>,
);

/// The resource and event head from the same authorized snapshot.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Success<T> {
    pub data: T,
    pub cursor: EventCursorDto,
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

impl EventCursorDto {
    /// Accepts event heads representable as SQLite signed integers.
    pub fn new(value: u64) -> Option<Self> {
        (value <= MAX_EVENT_CURSOR).then_some(Self(value))
    }
}

impl Serialize for EventCursorDto {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl UtcTimestampDto {
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

#[cfg(feature = "schema")]
fn event_cursor_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    // A shorter positive decimal is always below the limit. For a value of
    // equal length, each choice first differs from the limit at one digit.
    let limit = MAX_EVENT_CURSOR.to_string();
    let mut choices = vec![format!("[1-9][0-9]{{0,{}}}", limit.len() - 2)];
    for (index, digit) in limit.bytes().enumerate() {
        let minimum = if index == 0 { b'1' } else { b'0' };
        if digit > minimum {
            choices.push(format!(
                "{}[{}-{}][0-9]{{{}}}",
                &limit[..index],
                char::from(minimum),
                char::from(digit - 1),
                limit.len() - index - 1,
            ));
        }
    }
    choices.push(limit);
    schemars::json_schema!({
        "type": "string",
        "pattern": format!("^(0|{})$", choices.join("|"))
    })
}
