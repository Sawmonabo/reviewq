//! Independently validated public scalar representations.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::reports::limits::{MAX_DISCUSSION_ID_BYTES, MAX_ID_BYTES};

const MAX_SQLITE_INTEGER: u64 = i64::MAX as u64;
const MAX_JSON_INTEGER: u64 = 9_007_199_254_740_991;

macro_rules! string_dto {
    ($(#[$doc:meta])* $name:ident, $check:expr, $schema:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
        #[serde(transparent)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        pub struct $name(
            #[cfg_attr(feature = "schema", schemars(schema_with = $schema))] Box<str>,
        );

        impl $name {
            pub fn new(value: impl Into<Box<str>>) -> Option<Self> {
                let value = value.into();
                ($check)(&value).then_some(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = Box::<str>::deserialize(deserializer)?;
                Self::new(value).ok_or_else(|| de::Error::custom(concat!("invalid ", stringify!($name))))
            }
        }
    };
}

macro_rules! opaque_dto {
    ($(#[$doc:meta])* $name:ident) => {
        string_dto!($(#[$doc])* $name, |value: &str| bounded_text(value, 512), "opaque_schema");
    };
}

opaque_dto!(
    /// A registered run identifier.
    RunIdDto
);
opaque_dto!(
    /// The verified native session key within a run.
    SessionKeyDto
);
opaque_dto!(
    /// The client-provided stable retry key.
    SubmissionIdDto
);
opaque_dto!(
    /// An opaque review-bound patch revision, independent of its content hash.
    PatchRevisionDto
);
opaque_dto!(
    /// A file identity at a specified patch revision.
    FileIdDto
);
opaque_dto!(
    /// A signed handle for a displayed review checkpoint.
    CheckpointIdDto
);
opaque_dto!(
    /// A generation independent of the global event journal.
    HistoryGenerationDto
);
opaque_dto!(
    /// A visible source entry's provider-native identity.
    HistoryEntryIdDto
);
opaque_dto!(
    /// A provider-native turn in a scoped source.
    HistoryTurnIdDto
);
opaque_dto!(
    /// The public ID carried by a resource invalidation, interpreted by its kind.
    ResourceIdDto
);
opaque_dto!(
    /// A Jira issue key.
    TicketKeyDto
);
string_dto!(
    /// A report finding ID, such as `F-01`, as an outcome or assessment names it.
    FindingIdDto,
    |value: &str| bounded_text(value, MAX_ID_BYTES),
    "finding_id_schema"
);
string_dto!(
    /// A GitLab discussion ID of a selected-thread batch.
    DiscussionIdDto,
    |value: &str| bounded_text(value, MAX_DISCUSSION_ID_BYTES),
    "discussion_id_schema"
);

string_dto!(
    /// One opaque, path-safe ReviewQ review ID.
    ReviewIdDto,
    valid_review_id,
    "review_id_schema"
);
string_dto!(
    /// An opaque keyset continuation for list resources.
    PageCursorDto,
    |value: &str| bounded_text(value, 8192),
    "cursor_token_schema"
);
string_dto!(
    /// An opaque continuation for one bounded native history source.
    HistoryCursorDto,
    |value: &str| bounded_text(value, 8192),
    "cursor_token_schema"
);
string_dto!(
    /// A full SHA-1 or SHA-256 Git object name.
    GitShaDto,
    valid_git_sha,
    "git_sha_schema"
);
string_dto!(
    /// The strong quoted SHA-256 ETag of a complete patch.
    PatchETagDto,
    valid_etag,
    "etag_schema"
);

string_dto!(
    /// SHA-256 of exact report bytes or their declaration.
    ReportHashDto,
    |value: &str| value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
    "report_hash_schema"
);

#[cfg(feature = "schema")]
fn report_hash_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "string", "pattern": "^[0-9a-f]{64}$"})
}

macro_rules! decimal_dto {
    ($(#[$doc:meta])* $name:ident, $minimum:literal, $schema:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        pub struct $name(
            #[cfg_attr(feature = "schema", schemars(schema_with = $schema))] u64,
        );

        impl $name {
            pub fn new(value: u64) -> Option<Self> {
                ($minimum..=MAX_SQLITE_INTEGER).contains(&value).then_some(Self(value))
            }

            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = Box::<str>::deserialize(deserializer)?;
                read_decimal(&value).and_then(Self::new)
                    .ok_or_else(|| de::Error::custom(concat!("invalid ", stringify!($name))))
            }
        }
    };
}

decimal_dto!(
    /// A durable event cursor serialized as a canonical decimal string.
    EventCursorDto,
    0,
    "counter_schema"
);
decimal_dto!(
    /// A logical settings or decision revision.
    RevisionDto,
    0,
    "counter_schema"
);
decimal_dto!(
    /// A positive GitLab project identity.
    ProjectIdDto,
    1,
    "positive_decimal_schema"
);
decimal_dto!(
    /// A positive merge request IID within its GitLab project.
    MergeRequestIidDto,
    1,
    "positive_decimal_schema"
);
decimal_dto!(
    /// A positive upstream numeric identity in a compact projection.
    UpstreamIdDto,
    1,
    "positive_decimal_schema"
);

/// A nonnegative, exactly representable JSON count or byte offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SafeCountDto(
    #[cfg_attr(feature = "schema", schemars(schema_with = "count_schema"))] u64,
);

impl<'de> Deserialize<'de> for SafeCountDto {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u64::deserialize(deserializer)?)
            .ok_or_else(|| de::Error::custom("expected a nonnegative safe integer"))
    }
}

impl SafeCountDto {
    pub fn new(value: u64) -> Option<Self> {
        (value <= MAX_JSON_INTEGER).then_some(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn valid_review_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_git_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_etag(value: &str) -> bool {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

fn read_decimal(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.len() > 19
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

#[cfg(feature = "schema")]
fn opaque_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    bounded_text_schema(512)
}

#[cfg(feature = "schema")]
fn finding_id_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    bounded_text_schema(MAX_ID_BYTES)
}

#[cfg(feature = "schema")]
fn discussion_id_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    bounded_text_schema(MAX_DISCUSSION_ID_BYTES)
}

#[cfg(feature = "schema")]
fn cursor_token_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    bounded_text_schema(8192)
}

#[cfg(feature = "schema")]
fn bounded_text_schema(max_bytes: usize) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "pattern": r"^[^\u0000-\u001f\u007f-\u009f\ud800-\udfff]+$",
        "maxUtf8Bytes": max_bytes,
    })
}

#[cfg(feature = "schema")]
fn review_id_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({ "type": "string", "pattern": "^[A-Za-z0-9_-]{1,512}$" })
}

#[cfg(feature = "schema")]
fn git_sha_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({ "type": "string", "pattern": "^([0-9A-Fa-f]{40}|[0-9A-Fa-f]{64})$" })
}

#[cfg(feature = "schema")]
fn etag_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({ "type": "string", "pattern": r#"^"[0-9a-f]{64}"$"# })
}

#[cfg(feature = "schema")]
fn counter_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    decimal_schema(true)
}

#[cfg(feature = "schema")]
fn positive_decimal_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    decimal_schema(false)
}

#[cfg(feature = "schema")]
fn decimal_schema(include_zero: bool) -> schemars::Schema {
    // Shorter positive decimals fit. For equal length, each choice first
    // differs from the signed SQLite limit at one digit.
    let limit = MAX_SQLITE_INTEGER.to_string();
    let mut choices = Vec::new();
    if include_zero {
        choices.push("0".to_owned());
    }
    choices.push(format!("[1-9][0-9]{{0,{}}}", limit.len() - 2));
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
    schemars::json_schema!({ "type": "string", "pattern": format!("^({})$", choices.join("|")) })
}

#[cfg(feature = "schema")]
fn count_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({ "type": "integer", "minimum": 0, "maximum": MAX_JSON_INTEGER })
}
