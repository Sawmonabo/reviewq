//! The host identifies the native call. The arguments remain identical for every reviewer.

use reviewq_app::access::{Actor, AppError};
use reviewq_domain::session::HostProvider;
use reviewq_protocol::reports::{
    REPORT_HANDOFF_DESCRIPTION, REPORT_HANDOFF_SCHEMA, ReportHandoffInput, validate_report_handoff,
};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, RequestMetaObject, Tool};
use serde_json::{Value, json};

use crate::entry::Entry;
use crate::entry::features::reports::{McpReportProof, submit_verified};
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
        let message =
            serde_json::to_string(&json!({"error":"Invalid report handoff.","fieldErrors":errors}))
                .map_err(|_| AppError::Internal)?;
        return Ok(CallToolResult::error(vec![ContentBlock::text(message)]));
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
    let arguments = serde_json::to_vec(&raw).map_err(|_| AppError::InvalidRequest)?;
    let value: ReportHandoffInput =
        serde_json::from_value(raw).map_err(|_| AppError::InvalidRequest)?;
    let context = McpReportProof {
        actor: Some(actor),
        arguments,
        enclosing: enclosing.map(str::to_owned),
    };
    let verified = entry
        .workflow
        .proof
        .verify_mcp_report_call(
            lead,
            call.as_str().to_owned(),
            context.arguments.clone(),
            context.enclosing.clone(),
        )
        .await?;
    let receipt = submit_verified(entry, verified, value.handoff, false, Some(context)).await?;
    super::bounded_result(&serde_json::to_value(receipt.data).map_err(|_| AppError::Internal)?)
}
