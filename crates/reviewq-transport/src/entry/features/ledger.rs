//! The findings-ledger routes and the one handler every ledger tool call goes through. The
//! Claude plugin's bridge posts the six tool routes, the Codex hooks post `gate` and
//! `readback`, and the MCP server calls the same handler for Codex's tool calls, so a call is
//! checked, bound and answered the same way on every path.

mod cursor;
mod inputs;
mod results;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use reviewq_app::access::AppError;
use reviewq_app::ledger::{
    CallBinding, GateRequest, HookSession, HostSession, passes, publish, readback, record,
};
use reviewq_protocol::Success;
use reviewq_protocol::ledger::{
    self as dto, LEDGER_TOOLS, LedgerInput, LedgerTool, validate_ledger_input,
};
use serde::Serialize;
use serde_json::Value;

use crate::entry::error::ApiBytes;
use crate::entry::hosts::claude::require_companion;
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

/// Largest tool route body: a 128 KiB input with its binding. The Claude bridge answers a
/// larger input itself with the validator's own refusal.
const MAX_TOOL_BODY_BYTES: usize = 160 * 1024;
/// Largest `gate` or `readback` body.
const MAX_HOOK_BODY_BYTES: usize = 4 * 1024;

/// What one ledger tool returns, serialized as that tool's result.
#[derive(Serialize)]
#[serde(untagged)]
pub(in crate::entry) enum LedgerResult {
    Finding(dto::FindingReceipt),
    Note(dto::NoteReceipt),
    Page(dto::LedgerPage),
    Submitted(dto::SubmitReceipt),
    Held(dto::HeldReceipt),
}

/// The six tool routes, at `LedgerTool::route()` under `/native/ledger/`, and the two Codex
/// hook routes. Each requires the bridge credential.
pub(in crate::entry::features) fn routes() -> Router<Arc<Entry>> {
    let router = Router::new()
        .route("/native/ledger/gate", post(gate))
        .route("/native/ledger/readback", post(read_back_route));
    LEDGER_TOOLS.into_iter().fold(router, |router, tool| {
        router.route(
            &format!("/native/ledger/{}", tool.route()),
            post(
                move |State(entry): State<Arc<Entry>>,
                      headers: HeaderMap,
                      ApiBytes(bytes): ApiBytes| {
                    tool_route(entry, headers, bytes, tool)
                },
            )
            .layer(DefaultBodyLimit::max(MAX_TOOL_BODY_BYTES)),
        )
    })
}

/// Runs one ledger tool call for `binding`: checks `input` with the protocol's validator, maps
/// it to the app's request, makes the app call and maps its result.
///
/// Fails with `InvalidFields` listing every refused field, from the validator or from the
/// app, uncapped: the reply writer caps the list. A call the store cannot attribute to a
/// run's lead or a verified child fails with the app's error for it, and the app's other
/// errors pass through.
pub(in crate::entry) async fn call(
    entry: &Entry,
    binding: CallBinding,
    tool: LedgerTool,
    input: &Value,
) -> Result<LedgerResult, ApiError> {
    let input = validate_ledger_input(tool, input).map_err(ApiError::InvalidFields)?;
    let store = entry.store.as_ref();
    let endpoint = &entry.auth.endpoint;
    Ok(match input {
        LedgerInput::SaveFinding(input) => {
            let recorded = record::save_finding(store, binding, inputs::finding(input)?).await?;
            results::receipt(recorded.receipt)?
        }
        LedgerInput::SetFindingOutcome(input) => {
            let recorded =
                record::set_finding_outcome(store, binding, inputs::assessment(input)?).await?;
            results::receipt(recorded.receipt)?
        }
        LedgerInput::SaveSection(input) => {
            let recorded = record::save_section(store, binding, inputs::note(input)?).await?;
            results::receipt(recorded.receipt)?
        }
        LedgerInput::ReadDraft(input) => {
            let read = inputs::read(endpoint, input)?;
            let page = record::read_draft(store, binding, read).await?;
            LedgerResult::Page(results::page(endpoint, page)?)
        }
        LedgerInput::SubmitReview(input) => {
            let receipt = publish::submit_review(store, binding, inputs::submit(input)?).await?;
            LedgerResult::Submitted(results::submit(receipt)?)
        }
        LedgerInput::WaitForUser(input) => {
            let held = passes::wait_for_user(store, binding, input.question).await?;
            LedgerResult::Held(results::held(held))
        }
    })
}

/// Answers a host's stop hook with the gate's decision: a block with the reason the model
/// reads, or a release. A session that leads no review is released.
pub(in crate::entry) async fn answer_gate(
    entry: &Entry,
    request: GateRequest,
) -> Result<dto::GateReply, AppError> {
    let reason = passes::gate(entry.store.as_ref(), request).await?;
    Ok(reason.map_or_else(dto::GateReply::allow, dto::GateReply::block))
}

/// Answers a host's hook after compaction with the read-back text, which is empty when
/// `session` is `None` or leads no review.
pub(in crate::entry) async fn read_back(
    entry: &Entry,
    session: Option<HookSession>,
) -> Result<dto::ReadbackReply, AppError> {
    let text = match session {
        Some(session) => readback::read_back(entry.store.as_ref(), session).await?,
        None => None,
    };
    Ok(dto::ReadbackReply {
        text: text.unwrap_or_default(),
    })
}

/// A tool route: `{binding,input}` from the Claude plugin's bridge. Replies with the tool's
/// result, or the common error reply with `fieldErrors` for a refused input. A body that is
/// not a `LedgerRequest` is `InvalidRequest`.
async fn tool_route(
    entry: Arc<Entry>,
    headers: HeaderMap,
    bytes: Bytes,
    tool: LedgerTool,
) -> Result<Json<Success<LedgerResult>>, ApiError> {
    let request: dto::LedgerRequest =
        entry
            .auth
            .bridge_command(&headers, &bytes, MAX_TOOL_BODY_BYTES)?;
    let binding = claude_binding(request.binding)?;
    if let HostSession::Claude { session, .. } = &binding.session {
        require_companion(&entry, &headers, session).await?;
    }
    let result = call(&entry, binding, tool, &request.input).await?;
    Ok(Json(success(result, *entry.commits.borrow())?))
}

/// The Claude caller the plugin proves: the linked session, the subagent's ID when one calls,
/// the tool-use ID and the active turn.
fn claude_binding(binding: dto::LedgerBinding) -> Result<CallBinding, AppError> {
    Ok(CallBinding {
        session: HostSession::Claude {
            session: parse(binding.session_key.as_str())?,
            agent: binding.agent_id.map(|agent| agent.as_str().to_owned()),
        },
        call_id: binding.call_id.as_str().to_owned(),
        turn: binding
            .turn_id
            .map(|turn| parse(turn.as_str()))
            .transpose()?,
    })
}

/// `gate`: the Codex `Stop` hook asks whether the thread's turn may end.
async fn gate(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::GateReply>>, ApiError> {
    let request: dto::GateRequest =
        entry
            .auth
            .bridge_command(&headers, &bytes, MAX_HOOK_BODY_BYTES)?;
    let thread = parse(request.thread_id.as_str())?;
    entry.codex_hook_reports.record(&thread);
    let request = GateRequest {
        session: HookSession::Codex(thread),
        turn: request
            .turn_id
            .map(|turn| parse(turn.as_str()))
            .transpose()?,
        stop_hook_active: request.stop_hook_active,
    };
    let reply = answer_gate(&entry, request).await?;
    Ok(Json(success(reply, *entry.commits.borrow())?))
}

/// `readback`: the Codex `SessionStart` hook after compaction asks for the read-back.
async fn read_back_route(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::ReadbackReply>>, ApiError> {
    let request: dto::ReadbackRequest =
        entry
            .auth
            .bridge_command(&headers, &bytes, MAX_HOOK_BODY_BYTES)?;
    let thread = parse(request.thread_id.as_str())?;
    entry.codex_hook_reports.record(&thread);
    let session = HookSession::Codex(thread);
    let reply = read_back(&entry, Some(session)).await?;
    Ok(Json(success(reply, *entry.commits.borrow())?))
}
