use std::io::Write;
use std::path::Path;
use std::time::Duration;

use reviewq_app::hosts::{ClaudeQuestionAnswers, claude_answer_digest};
use reviewq_domain::identity::NativeSessionId;
use reviewq_protocol::ledger::{LedgerBinding, LedgerRequest, LedgerTool, MAX_TOOL_INPUT_BYTES};
use reviewq_protocol::{HistoryTurnIdDto, ResourceIdDto, SessionKeyDto};
use serde::Deserialize;

use super::hook::{Answer, INPUT_LIMIT, forward};
use super::{Failure, bounded, bridge, client, client_with_timeout, read_input};
use crate::runtime;

/// Room for the call's binding and envelope around a tool input of the largest size.
const LEDGER_CALL_LIMIT: usize = MAX_TOOL_INPUT_BYTES + 4096;
/// Largest ledger reply: a `review_ledger` page or a list of field errors, escaped.
const LEDGER_REPLY_LIMIT: usize = 256 * 1024;
/// The status check and the call together, for `finish_review`, which composes and publishes
/// a version; the plugin waits 60 seconds for it.
const FINISH_TIMEOUT: Duration = Duration::from_secs(55);
/// The status check and the call together, for every other tool: the default request time,
/// though the plugin stops waiting after 5 seconds.
const RECORD_TIMEOUT: Duration = Duration::from_secs(30);

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
            fields.insert("sessionKey".into(), session_key(&native).into());
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
    let endpoint = runtime::read_native_endpoint(profile)?;
    let client = if matches!(operation, ClaudeBridgeOperation::Question) {
        client_with_timeout(None)?
    } else {
        client()?
    };
    let result = bridge(&client, &endpoint, path, serde_json::to_vec(&payload)?).await?;
    let status = result.status();
    let body = bounded(result, 256 * 1024).await?;
    if !status.is_success() {
        let detail = match status.as_u16() {
            404 => "This Claude session has no verified ReviewQ binding.",
            409 => "Another ReviewQ companion owns this Claude session.",
            412 => "Reload the ReviewQ plugin in this Claude session.",
            _ => "ReviewQ could not record this Claude session observation.",
        };
        return Err(detail.into());
    }
    println!("{}", String::from_utf8(body)?);
    Ok(())
}

/// ReviewQ's key for a linked Claude session.
fn session_key(native: &NativeSessionId) -> String {
    format!("claude:{native}")
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
}

/// Sends one ledger tool call for the Claude session `native` to the tool's route and prints
/// the reply body unchanged, a result or a refusal with its field errors alike. It makes the
/// signed status check and that one call, within the tool's deadline, and nothing else. Fails,
/// with the message on stderr, when the call on stdin cannot be read or names no ledger tool,
/// when the listener is not this installation's ready service, or when ReviewQ cannot be
/// reached in time or gives no reply.
async fn ledger(profile: &Path, native: &NativeSessionId) -> Result<(), Failure> {
    let call: LedgerCall = serde_json::from_slice(&read_input(LEDGER_CALL_LIMIT)?)?;
    let tool = LedgerTool::from_name(&call.tool).ok_or("Unknown ReviewQ ledger tool.")?;
    let body = serde_json::to_vec(&LedgerRequest {
        binding: LedgerBinding {
            session_key: SessionKeyDto::new(session_key(native))
                .ok_or("Invalid Claude session.")?,
            call_id: call.call_id,
            turn_id: call.turn_id,
            agent_id: call.agent_id,
        },
        input: call.input,
    })?;
    let endpoint = runtime::read_native_endpoint(profile)?;
    let deadline = match tool {
        LedgerTool::FinishReview => FINISH_TIMEOUT,
        LedgerTool::RecordFinding
        | LedgerTool::AssessFinding
        | LedgerTool::RecordNote
        | LedgerTool::ReviewLedger
        | LedgerTool::NeedsInput => RECORD_TIMEOUT,
    };
    let client = client_with_timeout(Some(deadline))?;
    let path = format!("/api/v1/native/ledger/{}", tool.route());
    let reply = tokio::time::timeout(deadline, async {
        bounded(
            bridge(&client, &endpoint, &path, body).await?,
            LEDGER_REPLY_LIMIT,
        )
        .await
    })
    .await
    .map_err(|_| "ReviewQ did not answer this ledger call in time.")??;
    if reply.is_empty() {
        return Err("ReviewQ gave no reply to this ledger call.".into());
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&reply)?;
    stdout.write_all(b"\n")?;
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
    turn_id: Option<String>,
    message_id: Option<String>,
    index: Option<u64>,
    #[serde(rename = "final")]
    final_chunk: Option<bool>,
    stop_hook_active: Option<bool>,
    source: Option<String>,
}

/// Forwards one Claude hook of the linked session's own agent to the hook route. For `Stop`
/// it prints the gate's block, and for a `SessionStart` after compaction the read-back text,
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
        "MessageDisplay" if hook.index == Some(0) || hook.final_chunk == Some(true) => (
            serde_json::json!({
                "kind": "display",
                "turnId": hook.turn_id.ok_or("Missing native turn.")?,
                "messageId": hook.message_id.ok_or("Missing native display message.")?,
                "index": hook.index.ok_or("Missing native display sequence.")?,
                "finalChunk": hook.final_chunk.unwrap_or(false),
            }),
            None,
        ),
        "Stop" => (
            serde_json::json!({
                "kind": "stopping",
                "stopHookActive": hook.stop_hook_active.unwrap_or(false),
            }),
            Some(Answer::Gate),
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
