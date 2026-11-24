//! The host identifies the native call. The arguments remain identical for every reviewer.

use reviewq_app::access::{Actor, AppError};
use reviewq_domain::session::HostProvider;
use reviewq_protocol::reports::{
    REPORT_HANDOFF_DESCRIPTION, REPORT_HANDOFF_SCHEMA, ReportHandoffFieldError, ReportHandoffInput,
    report_amendment_source_error, validate_report_handoff,
};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, RequestMetaObject, Tool};
use serde_json::{Value, json};

use crate::entry::Entry;
use crate::entry::features::reports::{McpReportProof, canonical_arguments, submit_verified};
use crate::entry::pages::parse;

const NAME: &str = "report_handoff";

pub(super) fn tool() -> Tool {
    let schema = serde_json::from_str::<JsonObject>(REPORT_HANDOFF_SCHEMA)
        .expect("the generated report handoff schema is a JSON object");
    Tool::new(NAME, REPORT_HANDOFF_DESCRIPTION, schema)
}

pub(super) async fn submit(
    entry: &Entry,
    actor: Actor,
    meta: &RequestMetaObject,
    input: JsonObject,
) -> Result<CallToolResult, AppError> {
    let raw = Value::Object(input);
    let errors = validate_report_handoff(&raw);
    if !errors.is_empty() {
        return invalid_handoff(&errors);
    }
    let field = |key| meta.get(key).and_then(Value::as_str);
    let session = field("threadId").ok_or(AppError::Forbidden)?;
    let call = field("callId").ok_or(AppError::Forbidden)?;
    let enclosing = field("itemId");
    let call = reviewq_protocol::ResourceIdDto::new(call).ok_or(AppError::InvalidRequest)?;
    let lead = entry
        .store
        .mcp_report_lead(actor.clone(), HostProvider::Codex, parse(session)?)
        .await?;
    let arguments = canonical_arguments(raw)?;
    let value: ReportHandoffInput =
        serde_json::from_slice(&arguments).map_err(|_| AppError::InvalidRequest)?;
    let witness =
        entry
            .report_witnesses
            .take_proof(lead.session_key(), call.as_str(), &arguments)?;
    let context = McpReportProof {
        actor: Some(actor),
        arguments,
        enclosing: enclosing.map(str::to_owned),
        witness,
    };
    let verified = context.verify(entry, lead, call.as_str()).await?;
    let receipt = match submit_verified(entry, verified, value.handoff, false, Some(context)).await
    {
        Err(AppError::ReportAmendmentSourceChanged) => {
            return invalid_handoff(&[report_amendment_source_error()]);
        }
        Err(AppError::InvalidHandoff(errors)) => {
            return invalid_handoff(&crate::entry::error::handoff_field_errors(errors));
        }
        result => result?,
    };
    super::bounded_result(&serde_json::to_value(receipt.data).map_err(|_| AppError::Internal)?)
}

fn invalid_handoff(errors: &[ReportHandoffFieldError]) -> Result<CallToolResult, AppError> {
    let message =
        serde_json::to_string(&json!({"error":"Invalid report handoff.","fieldErrors":errors}))
            .map_err(|_| AppError::Internal)?;
    if message.len() > super::MAX_TOOL_RESPONSE {
        return Err(AppError::PayloadTooLarge);
    }
    Ok(CallToolResult::error(vec![ContentBlock::text(message)]))
}
