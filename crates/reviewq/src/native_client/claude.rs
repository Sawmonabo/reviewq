use std::io::Write;
use std::path::Path;

use reviewq_app::hosts::{ClaudeHost, ClaudeQuestionAnswers, claude_answer_digest};
use reviewq_domain::identity::NativeSessionId;
use reviewq_domain::session::HostProvider;
use reviewq_protocol::deadlines::tool_deadline;
use reviewq_protocol::ledger::{
    LedgerBinding, LedgerRequest, LedgerTool, MAX_LEDGER_RESULT_BYTES, MAX_TOOL_INPUT_BYTES,
    oversized_input,
};
use reviewq_protocol::{ErrorCode, HistoryTurnIdDto, ResourceIdDto, SessionKeyDto};
use reviewq_transport::{ServiceEndpoint, field_error_text, ledger_refusal_text};
use serde::Deserialize;

use super::hook::{Answer, GateHost, INPUT_LIMIT, forward};
use super::{Failure, bounded, bridge_as, client, client_with_timeout, read_input, read_within};
use crate::runtime;

/// Room for the call's other fields, each ID at most 512 bytes, around a tool input of the
/// largest size. A call above it carries an input above `MAX_TOOL_INPUT_BYTES` as compact JSON.
const LEDGER_CALL_LIMIT: usize = MAX_TOOL_INPUT_BYTES + 4096;
/// Largest ledger reply: a result of at most `MAX_LEDGER_RESULT_BYTES` in the reply envelope,
/// or a refusal, which is smaller.
const LEDGER_REPLY_LIMIT: usize = MAX_LEDGER_RESULT_BYTES + 1024;

#[derive(Clone, clap::ValueEnum)]
pub(crate) enum ClaudeBridgeOperation {
    Heartbeat,
    DeliveryCheck,
    Hook,
    Question,
    QuestionFinished,
    Action,
    ActionStatus,
    Ledger,
}

pub(crate) async fn claude_bridge(
    profile: &Path,
    operation: ClaudeBridgeOperation,
    session_id: String,
) -> Result<(), Failure> {
    let native: NativeSessionId = session_id.parse()?;
    let (path, limit) = match operation {
        ClaudeBridgeOperation::Ledger => return ledger(profile, &native).await,
        ClaudeBridgeOperation::Heartbeat => ("/api/v1/native/claude/heartbeat", 16_384usize),
        ClaudeBridgeOperation::DeliveryCheck => ("/api/v1/native/claude/delivery-check", 16_384),
        ClaudeBridgeOperation::Hook => ("/api/v1/native/claude/hook", 16_384),
        ClaudeBridgeOperation::Question => ("/api/v1/native/claude/question", 64 * 1024),
        ClaudeBridgeOperation::QuestionFinished => {
            ("/api/v1/native/claude/question-finished", 128 * 1024)
        }
        ClaudeBridgeOperation::Action => ("/api/v1/native/actions/declare", 80 * 1024),
        ClaudeBridgeOperation::ActionStatus => ("/api/v1/native/actions/reconcile", 16_384),
    };
    let mut payload: serde_json::Value = serde_json::from_slice(&read_input(limit)?)?;
    let fields = payload
        .as_object_mut()
        .ok_or("Invalid ReviewQ companion payload.")?;
    if fields.contains_key("context") {
        return Err("ReviewQ companion context is supplied by the native client.".into());
    }
    let proof = fields.remove("proof");
    if matches!(operation, ClaudeBridgeOperation::QuestionFinished) {
        fields.remove("answerDigest");
        if let Some(answers) = fields.remove("nativeAnswers")
            && let Ok(answers) = serde_json::from_value::<ClaudeQuestionAnswers>(answers)
            && let Some(digest) = claude_answer_digest(&answers)
        {
            fields.insert(
                "answerDigest".into(),
                serde_json::Value::String(digest.to_string()),
            );
        }
    }
    match operation {
        ClaudeBridgeOperation::Action | ClaudeBridgeOperation::ActionStatus => {
            fields.insert("sessionKey".into(), session_key(&native)?.into());
        }
        _ => {
            fields.insert(
                "context".into(),
                serde_json::json!({
                    "protocolVersion": 1,
                    "sessionId": native.as_str(),
                    "cwd": std::env::current_dir()?.canonicalize()?,
                    "profile": runtime::claude_home()?.canonicalize()?,
                }),
            );
        }
    }
    if matches!(operation, ClaudeBridgeOperation::Heartbeat) {
        fields.insert("host".into(), host());
    }
    let endpoint = runtime::read_native_endpoint(profile)?;
    let headers = companion_headers(&endpoint, proof)?;
    let body = serde_json::to_vec(&payload)?;
    let (status, body) = match operation {
        // `review_action` answers within its tool's deadline, the one limit on the call, so a
        // slow GitLab read ends with its message.
        ClaudeBridgeOperation::Action | ClaudeBridgeOperation::ActionStatus => {
            let deadline = tool_deadline("review_action");
            tokio::time::timeout(
                deadline.limit,
                post(&client_with_timeout(None)?, &endpoint, path, body, headers),
            )
            .await
            .map_err(|_| deadline.timed_out)??
        }
        ClaudeBridgeOperation::Question => {
            post(&client_with_timeout(None)?, &endpoint, path, body, headers).await?
        }
        _ => post(&client()?, &endpoint, path, body, headers).await?,
    };
    if !status.is_success() {
        let detail = match operation {
            ClaudeBridgeOperation::Action => action_refusal(true, &body),
            ClaudeBridgeOperation::ActionStatus => action_refusal(false, &body),
            _ => match status.as_u16() {
                404 => "This Claude session has no verified ReviewQ binding.",
                409 => "Another ReviewQ companion owns this Claude session.",
                412 => "Reload the ReviewQ plugin in this Claude session.",
                _ => return Err(unrecorded(&body).into()),
            },
        };
        return Err(detail.into());
    }
    println!("{}", String::from_utf8(body)?);
    Ok(())
}

/// The process hosting the Claude session, which is the helper's parent, for a heartbeat:
/// `{pid,startTime,bootId,pidNamespace}`, or null when it cannot be read. It replaces any
/// `host` the plugin sent.
fn host() -> serde_json::Value {
    ClaudeHost::parent().map_or(serde_json::Value::Null, |host| {
        serde_json::json!({
            "pid": host.pid,
            "startTime": host.start_time,
            "bootId": host.boot_id,
            "pidNamespace": host.pid_namespace,
        })
    })
}

/// The bridge headers for a plugin request, with the companion proof it carried. The plugin
/// sends the proof on standard input only, and it travels on as a header, never in a body or
/// argv. A request with none, from a session with no companion yet, goes without it, and
/// ReviewQ refuses it for a session it stores.
fn companion_headers(
    endpoint: &ServiceEndpoint,
    proof: Option<serde_json::Value>,
) -> Result<reqwest::header::HeaderMap, Failure> {
    Ok(match proof {
        None => endpoint.bridge_headers()?,
        Some(serde_json::Value::String(proof)) => endpoint.companion_headers(&proof)?,
        Some(_) => return Err("Invalid ReviewQ companion proof.".into()),
    })
}

/// One bridge request with `headers` after the signed status check: the reply's status and its
/// body.
async fn post(
    client: &reqwest::Client,
    endpoint: &ServiceEndpoint,
    path: &str,
    body: Vec<u8>,
    headers: reqwest::header::HeaderMap,
) -> Result<(reqwest::StatusCode, Vec<u8>), Failure> {
    let response = bridge_as(client, endpoint, path, body, headers).await?;
    let status = response.status();
    Ok((status, bounded(response, 256 * 1024).await?))
}

/// What the model reads when ReviewQ refuses `review_action`, from the refusal's error code:
/// a declaration when `declare`, else a reconcile. A refused declaration stores nothing.
/// The refusal of an observation the service did not record, with the service's own reason so
/// a debug log shows which check failed.
fn unrecorded(reply: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Refusal {
        error: Reason,
    }
    #[derive(Deserialize)]
    struct Reason {
        message: String,
    }
    let reason = serde_json::from_slice::<Refusal>(reply).map(|refusal| refusal.error.message);
    match reason {
        Ok(reason) => format!("ReviewQ could not record this Claude session observation: {reason}"),
        Err(_) => "ReviewQ could not record this Claude session observation.".to_owned(),
    }
}

fn action_refusal(declare: bool, reply: &[u8]) -> &'static str {
    #[derive(Deserialize)]
    struct Refusal {
        error: Code,
    }
    #[derive(Deserialize)]
    struct Code {
        code: ErrorCode,
    }
    let code = serde_json::from_slice::<Refusal>(reply).map(|refusal| refusal.error.code);
    match (code, declare) {
        (Ok(ErrorCode::InvalidRequest), true) => {
            "ReviewQ refused this declaration, so nothing was recorded. It needs intent and reviewedSha, the merge request's full head SHA; comments and both take 1 to 50 distinct comments, and approval and cancel take none. Correct it and declare the action again."
        }
        (Ok(ErrorCode::InvalidRequest), false) => {
            "ReviewQ could not read this reconcile call. Reload the ReviewQ plugin in this Claude session."
        }
        (Ok(ErrorCode::Unauthorized), _) => {
            "ReviewQ did not accept the plugin's credential. Check the installed service and plugin."
        }
        (Ok(ErrorCode::Forbidden), _) => {
            "ReviewQ takes a publication action only in a turn that the user's latest instruction started, typed in this conversation or sent from ReviewQ. This turn did not start that way, so nothing was recorded. Ask the user for the instruction."
        }
        (Ok(ErrorCode::NotFound), true) => {
            "ReviewQ found no open review_action call in the current turn of this conversation's linked review, or a comment names a discussion that is not on the merge request, so nothing was recorded."
        }
        (Ok(ErrorCode::NotFound), false) => {
            "ReviewQ found no open review_action call in the current turn of this conversation's linked review."
        }
        (Ok(ErrorCode::StaleRevision), true) => {
            "The merge request no longer matches this declaration: it is not open at reviewedSha, or ReviewQ's settings changed. Nothing was recorded. Read the merge request again before declaring the action."
        }
        (Ok(ErrorCode::StaleRevision), false) => {
            "The action changed while ReviewQ checked it. Call review_action with operation reconcile again to read it."
        }
        (Ok(ErrorCode::Conflict), true) => {
            "Something newer stands in this declaration's way: the merge request is not open, the user has a newer instruction waiting, a later action is declared, or this turn changed while ReviewQ checked it. Nothing was recorded. Call review_action with operation reconcile to read the current action."
        }
        (Ok(ErrorCode::Conflict), false) => {
            "The merge request or the GitLab user changed since the action was declared, the user has a newer instruction waiting, or this turn changed while ReviewQ checked it."
        }
        (Ok(ErrorCode::IdempotencyConflict), _) => {
            "This review_action call already declared a different action, so nothing new was recorded. Call review_action with operation reconcile to read it."
        }
        (Ok(ErrorCode::PayloadTooLarge), _) => {
            "This action, or the transcript entry ReviewQ read to check it, is larger than ReviewQ accepts."
        }
        (Ok(ErrorCode::Busy), _) => {
            "ReviewQ cannot settle this action yet, so nothing was recorded. Call review_action again in a moment."
        }
        (Ok(ErrorCode::Unavailable), _) => {
            "ReviewQ could not read GitLab or this conversation's transcript, or the merge request's project is no longer configured, so it could not check the action. Check the GitLab connection and project settings, then call review_action again."
        }
        _ => {
            "ReviewQ failed while checking this action. Call review_action with operation reconcile before declaring the action again."
        }
    }
}

/// ReviewQ's key for a linked Claude session.
fn session_key(native: &NativeSessionId) -> Result<String, Failure> {
    Ok(HostProvider::Claude.session_key(native)?.to_string())
}

/// One ledger tool call as the plugin writes it on stdin.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LedgerCall {
    tool: String,
    call_id: ResourceIdDto,
    turn_id: Option<HistoryTurnIdDto>,
    agent_id: Option<ResourceIdDto>,
    input: serde_json::Value,
    proof: Option<serde_json::Value>,
}

/// Sends one ledger tool call for the Claude session `native` to the tool's route and prints
/// what the plugin returns to the model: the reply body unchanged for a result, whose `data`
/// is the tool's result, or `{"error":text}` for a refusal, with the text the MCP server gives
/// Codex for the same refusal. It makes the signed status check and that one call, within the
/// tool's deadline, and nothing else. A call too large to carry gets the validator's refusal
/// without either request. Fails, with the message on stderr, when the call on stdin cannot be
/// read or names no ledger tool, when the listener is not this installation's ready service,
/// when ReviewQ cannot be reached or gives no readable reply, or with the tool's timeout
/// sentence when its deadline passes.
async fn ledger(profile: &Path, native: &NativeSessionId) -> Result<(), Failure> {
    let Some(call) = read_within(LEDGER_CALL_LIMIT)? else {
        return print_refusal(&field_error_text(&[oversized_input()]));
    };
    let call: LedgerCall = serde_json::from_slice(&call)?;
    let tool = LedgerTool::from_name(&call.tool).ok_or("Unknown ReviewQ ledger tool.")?;
    let body = serde_json::to_vec(&LedgerRequest {
        binding: LedgerBinding {
            session_key: SessionKeyDto::new(session_key(native)?)
                .ok_or("Invalid Claude session.")?,
            call_id: call.call_id,
            turn_id: call.turn_id,
            agent_id: call.agent_id,
        },
        input: call.input,
    })?;
    let endpoint = runtime::read_native_endpoint(profile)?;
    let headers = companion_headers(&endpoint, call.proof)?;
    // The tool's deadline is the one limit, so a slow call always ends with its message.
    let client = client_with_timeout(None)?;
    let path = format!("/api/v1/native/ledger/{}", tool.route());
    let deadline = tool_deadline(tool.name());
    let (status, reply) = tokio::time::timeout(deadline.limit, async {
        let response = bridge_as(&client, &endpoint, &path, body, headers).await?;
        let status = response.status();
        Ok::<_, Failure>((status, bounded(response, LEDGER_REPLY_LIMIT).await?))
    })
    .await
    .map_err(|_| deadline.timed_out)??;
    if !status.is_success() {
        let text = ledger_refusal_text(&reply)
            .ok_or("ReviewQ gave no readable reply to this ledger call.")?;
        return print_refusal(&text);
    }
    if reply.is_empty() {
        return Err("ReviewQ gave no reply to this ledger call.".into());
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&reply)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

/// Prints a refused ledger call as the plugin returns it to the model.
fn print_refusal(text: &str) -> Result<(), Failure> {
    println!("{}", serde_json::json!({ "error": text }));
    Ok(())
}

#[derive(Deserialize)]
struct ClassicInput {
    session_id: String,
    transcript_path: Option<std::path::PathBuf>,
    cwd: std::path::PathBuf,
    hook_event_name: String,
    agent_id: Option<String>,
    notification_type: Option<String>,
    tool_name: Option<String>,
    index: Option<u64>,
    #[serde(rename = "final")]
    final_chunk: Option<bool>,
    stop_hook_active: Option<bool>,
    source: Option<String>,
}

/// Forwards one Claude hook of the linked session's own agent to the hook route. For `Stop`
/// it prints the gate's hold as hook feedback, and for a `SessionStart` after compaction the read-back text,
/// through `hook::forward`; other events print nothing. Hooks fired inside a subagent, other
/// `SessionStart` sources and unlisted events are not forwarded. Fails on unreadable input or
/// when ReviewQ does not answer; the caller ignores the failure so the session is never broken
/// by this hook.
pub(crate) async fn claude_hook(profile: &Path) -> Result<(), Failure> {
    let Some(bound) = std::env::var_os("REVIEWQ_CLAUDE_LINKED") else {
        return Ok(());
    };
    let hook: ClassicInput = serde_json::from_slice(&read_input(INPUT_LIMIT)?)?;
    if bound != hook.session_id.as_str() || hook.agent_id.is_some() {
        return Ok(());
    }
    let session: NativeSessionId = hook.session_id.parse()?;
    let (event, answer) = match hook.hook_event_name.as_str() {
        "PermissionRequest" if hook.tool_name.as_deref() != Some("AskUserQuestion") => {
            (serde_json::json!({"kind": "permission_pending"}), None)
        }
        "Notification" if hook.notification_type.as_deref() == Some("permission_prompt") => {
            (serde_json::json!({"kind": "permission_pending"}), None)
        }
        "MessageDisplay" if hook.index == Some(0) || hook.final_chunk == Some(true) => {
            (serde_json::json!({"kind": "display"}), None)
        }
        "Stop" => (
            serde_json::json!({
                "kind": "stopping",
                "stopHookActive": hook.stop_hook_active.unwrap_or(false),
            }),
            Some(Answer::Gate(GateHost::Claude)),
        ),
        "SessionStart" if hook.source.as_deref() == Some("compact") => (
            serde_json::json!({"kind": "compacted"}),
            Some(Answer::Readback),
        ),
        _ => return Ok(()),
    };
    let body = serde_json::to_vec(&serde_json::json!({
        "context": {
            "protocolVersion": 1, "sessionId": session.as_str(), "cwd": hook.cwd.canonicalize()?,
            "profile": runtime::claude_home()?.canonicalize()?, "transcript": hook.transcript_path,
        },
        "classic": event,
    }))?;
    forward(profile, "/api/v1/native/claude/hook", body, answer).await
}
