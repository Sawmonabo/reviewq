//! Exact native MCP report arguments and returned receipt shared by the host readers.
use reviewq_app::access::AppError;
use reviewq_app::workflow::ExpectedAcknowledgement;
use reviewq_domain::identity::ReportHash;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) fn native_arguments_hash(mut arguments: Value) -> Result<ReportHash, AppError> {
    arguments.sort_all_objects();
    let bytes = serde_json::to_vec(&arguments).map_err(|_| AppError::InvalidRequest)?;
    Ok(ReportHash::from_sha256(Sha256::digest(bytes).into()))
}

pub(super) fn acknowledgement(content: &Value, expected: &ExpectedAcknowledgement) -> bool {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Ack {
        receipt_id: String,
        state: String,
        declaration_sha256: String,
    }
    let text = match content {
        Value::String(text) => text.as_str(),
        Value::Array(items) if items.len() == 1 && items[0]["type"] == "text" => {
            let Some(text) = items[0]["text"].as_str() else {
                return false;
            };
            text
        }
        _ => return false,
    };
    text.len() <= 1_024
        && serde_json::from_str::<Ack>(text).is_ok_and(|value| {
            value.receipt_id == expected.receipt_id.as_str()
                && value.declaration_sha256 == expected.declaration_sha256.as_str()
                && matches!(value.state.as_str(), "pending_completion" | "recorded")
        })
}
