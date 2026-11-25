//! The six ledger tools for Codex clients. A Codex tool call names its thread and call in the
//! request's `_meta`, as Codex's `mcp_tool_call.rs` writes them, and its turn and parent thread
//! in the turn metadata object under `x-codex-turn-metadata`. A Claude client files through
//! its plugin instead, so it is not offered the tools.

use std::str::FromStr;

use axum::http::{header, request::Parts};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::ledger::{CallBinding, HostSession};
use reviewq_protocol::ResourceIdDto;
use reviewq_protocol::ledger::{LEDGER_TOOLS, LedgerTool};
use rmcp::RoleServer;
use rmcp::model::{JsonObject, RequestMetaObject, Tool};
use rmcp::service::RequestContext;
use serde_json::Value;

use crate::entry::features::ledger::{self, LedgerResult};
use crate::entry::{ApiError, Entry};

/// `_meta` key of the calling Codex thread.
const THREAD_ID: &str = "threadId";
/// `_meta` key of Codex's ID for this tool call.
const CALL_ID: &str = "callId";
/// `_meta` key of the turn metadata object.
const TURN_METADATA: &str = "x-codex-turn-metadata";
/// Turn metadata key of the turn the call runs in.
const TURN_ID: &str = "turn_id";
/// Turn metadata key of the thread that spawned a subagent's thread.
const PARENT_THREAD_ID: &str = "parent_thread_id";
/// How Claude Code names itself, as its MCP client's `User-Agent` prefix and client name.
const CLAUDE_CLIENT: &str = "claude-code";

/// The six tools as the MCP server lists them: name, description and generated input schema.
pub(super) fn tools() -> impl Iterator<Item = Tool> {
    LEDGER_TOOLS
        .into_iter()
        .map(|tool| Tool::new(tool.name(), tool.description(), tool.input_schema().clone()))
}

/// Whether the request comes from Claude Code, by its `User-Agent` or the client information
/// the request carries. Any other client is offered the ledger tools.
pub(super) fn claude_client(context: &RequestContext<RoleServer>) -> bool {
    let agent = context
        .extensions
        .get::<Parts>()
        .and_then(|parts| parts.headers.get(header::USER_AGENT))
        .and_then(|value| value.to_str().ok());
    agent.is_some_and(|agent| {
        agent
            .strip_prefix(CLAUDE_CLIENT)
            .is_some_and(|rest| rest.starts_with('/'))
    }) || context
        .client_info()
        .is_some_and(|client| client.name == CLAUDE_CLIENT)
}

/// Runs one ledger tool call from Codex through the handler the Claude routes use. The caller
/// is the thread `_meta` names, with its parent thread from the turn metadata when present and
/// the request's checkout grant.
///
/// Fails with `Forbidden` when `_meta` names no thread or no call, because such a call cannot
/// be attributed, and with `InvalidRequest` for a malformed thread or turn ID. Otherwise fails
/// as the handler does.
pub(super) async fn call(
    entry: &Entry,
    tool: LedgerTool,
    meta: &RequestMetaObject,
    actor: Actor,
    input: JsonObject,
) -> Result<LedgerResult, ApiError> {
    let binding = binding(meta, actor)?;
    ledger::call(entry, binding, tool, &Value::Object(input)).await
}

fn binding(meta: &RequestMetaObject, actor: Actor) -> Result<CallBinding, AppError> {
    let thread = meta
        .get(THREAD_ID)
        .and_then(Value::as_str)
        .ok_or(AppError::Forbidden)?;
    let call_id = meta
        .get(CALL_ID)
        .and_then(Value::as_str)
        .and_then(ResourceIdDto::new)
        .ok_or(AppError::Forbidden)?;
    let turn_metadata = meta.get(TURN_METADATA).and_then(Value::as_object);
    Ok(CallBinding {
        session: HostSession::Codex {
            thread: thread.parse().map_err(|_| AppError::InvalidRequest)?,
            parent: turn_field(turn_metadata, PARENT_THREAD_ID)?,
            actor,
        },
        call_id: call_id.as_str().to_owned(),
        turn: turn_field(turn_metadata, TURN_ID)?,
    })
}

/// A string field of the turn metadata, parsed; `None` when the metadata or the field is
/// absent.
fn turn_field<T: FromStr>(metadata: Option<&JsonObject>, key: &str) -> Result<Option<T>, AppError> {
    metadata
        .and_then(|metadata| metadata.get(key))
        .and_then(Value::as_str)
        .map(|value| value.parse().map_err(|_| AppError::InvalidRequest))
        .transpose()
}
