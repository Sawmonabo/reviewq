use serde::Serialize;

use crate::{Success, UtcTimestampDto};

const MAX_ID_BYTES: usize = 512;

/// The addressed receipt for the message send and receipt lookup routes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DeliveryReceipt {
    pub submission_id: SubmissionIdDto,
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    pub state: DeliveryState,
    pub created_at: UtcTimestampDto,
    pub updated_at: UtcTimestampDto,
}

/// The snapshot returned by message send and receipt lookup.
pub type DeliveryReceiptResponse = Success<DeliveryReceipt>;

/// The known durable state of one message submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DeliveryState {
    Queued,
    Dispatching,
    Started,
    Rejected,
    Uncertain,
}

macro_rules! delivery_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
        #[serde(transparent)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        pub struct $name(
            #[cfg_attr(feature = "schema", schemars(schema_with = "id_schema"))] Box<str>,
        );

        impl $name {
            /// Accepts a nonempty identifier up to 512 UTF-8 bytes with no controls.
            pub fn new(value: impl Into<Box<str>>) -> Option<Self> {
                let value = value.into();
                let valid = !value.is_empty()
                    && value.len() <= MAX_ID_BYTES
                    && !value.chars().any(char::is_control);
                valid.then_some(Self(value))
            }
        }
    };
}

delivery_id!(
    /// The client-supplied submission identifier scoped to the operation.
    SubmissionIdDto
);
delivery_id!(
    /// The registered run identifier.
    RunIdDto
);
delivery_id!(
    /// The exact native session key registered for a run.
    SessionKeyDto
);

#[cfg(feature = "schema")]
fn id_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "pattern": r"^[^\u0000-\u001f\u007f-\u009f\ud800-\udfff]+$",
        "maxUtf8Bytes": MAX_ID_BYTES,
    })
}
