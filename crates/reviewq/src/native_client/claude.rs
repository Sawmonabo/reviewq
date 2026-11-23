use std::io::Read;
use std::path::Path;

use reviewq_app::hosts::{ClaudeQuestionAnswers, claude_answer_digest};
use reviewq_domain::identity::NativeSessionId;

use super::{Failure, bounded, bridge, client, client_with_timeout, signed, verify_ready};
use crate::runtime;

#[derive(Clone, clap::ValueEnum)]
pub(crate) enum ClaudeBridgeOperation {
    Heartbeat,
    DeliveryCheck,
    Hook,
    Question,
    QuestionFinished,
    Report,
    ReportStatus,
    Action,
    ActionStatus,
}

pub(crate) async fn claude_bridge(
    profile: &Path,
    operation: ClaudeBridgeOperation,
    session_id: String,
) -> Result<(), Failure> {
    let native: NativeSessionId = session_id.parse()?;
    let (path, limit) = match operation {
        ClaudeBridgeOperation::Heartbeat => ("/api/v1/native/claude/heartbeat", 16_384usize),
        ClaudeBridgeOperation::DeliveryCheck => ("/api/v1/native/claude/delivery-check", 16_384),
        ClaudeBridgeOperation::Hook => ("/api/v1/native/claude/hook", 16_384),
        ClaudeBridgeOperation::Question => ("/api/v1/native/claude/question", 64 * 1024),
        ClaudeBridgeOperation::QuestionFinished => {
            ("/api/v1/native/claude/question-finished", 128 * 1024)
        }
        ClaudeBridgeOperation::Report => ("/api/v1/native/reports/handoff", 144 * 1024),
        ClaudeBridgeOperation::ReportStatus => {
            ("/api/v1/native/reports/handoff-status", 144 * 1024)
        }
        ClaudeBridgeOperation::Action => ("/api/v1/native/actions/declare", 80 * 1024),
        ClaudeBridgeOperation::ActionStatus => ("/api/v1/native/actions/reconcile", 16_384),
    };
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .take((limit + 1) as u64)
        .read_to_end(&mut input)?;
    if input.len() > limit {
        return Err("ReviewQ companion input exceeds its limit.".into());
    }
    let mut payload: serde_json::Value = serde_json::from_slice(&input)?;
    drop(input);
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
        ClaudeBridgeOperation::Report | ClaudeBridgeOperation::ReportStatus => {
            let turn = fields
                .remove("turnId")
                .ok_or("Missing native Claude turn.")?;
            let call = fields
                .remove("callId")
                .ok_or("Missing native Claude call.")?;
            let witness = fields.remove("witnessOrdinal");
            if witness.as_ref().is_some_and(|value| {
                !value
                    .as_u64()
                    .is_some_and(|ordinal| (1..=u64::from(u32::MAX)).contains(&ordinal))
            }) {
                return Err("Invalid native Claude report call ordinal.".into());
            }
            if matches!(operation, ClaudeBridgeOperation::Report) {
                let input = serde_json::Value::Object(std::mem::take(fields));
                let errors = reviewq_protocol::reports::validate_report_handoff(&input);
                if !errors.is_empty() {
                    println!(
                        "{}",
                        serde_json::json!({
                            "error": "Invalid report handoff.",
                            "fieldErrors": errors,
                            "correctionRequested": true,
                        })
                    );
                    return Ok(());
                }
                if let serde_json::Value::Object(input) = input {
                    *fields = input;
                }
            }
            let mut context = serde_json::json!({
                "sessionKey": format!("claude:{native}"), "turnId": turn, "callId": call
            });
            if let Some(witness) = witness {
                context["witnessOrdinal"] = witness;
            }
            fields.insert("context".into(), context);
        }
        ClaudeBridgeOperation::Action | ClaudeBridgeOperation::ActionStatus => {
            fields.insert(
                "sessionKey".into(),
                serde_json::json!(format!("claude:{native}")),
            );
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
    let status = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &status)?;
    let result = bridge(&client, &endpoint, path, serde_json::to_vec(&payload)?).await?;
    let status = result.status();
    let body = bounded(result, 256 * 1024).await?;
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct InvalidReport {
        field_errors: Vec<reviewq_protocol::reports::ReportHandoffFieldError>,
    }
    if status == reqwest::StatusCode::BAD_REQUEST
        && matches!(operation, ClaudeBridgeOperation::Report)
        && let Ok(response) = serde_json::from_slice::<InvalidReport>(&body)
        && !response.field_errors.is_empty()
    {
        println!(
            "{}",
            serde_json::json!({
                "error": "Invalid report handoff.",
                "fieldErrors": response.field_errors,
                "correctionRequested": true,
            })
        );
        return Ok(());
    }
    if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
        && matches!(operation, ClaudeBridgeOperation::Report)
    {
        println!(
            "{}",
            serde_json::json!({
                "error":reviewq_app::access::REPORT_CORRECTION_MESSAGE,
                "correctionRequested":true,
            })
        );
        return Ok(());
    }
    if !status.is_success() {
        let report = matches!(
            operation,
            ClaudeBridgeOperation::Report | ClaudeBridgeOperation::ReportStatus
        );
        let detail = match (status.as_u16(), report) {
            (404, true) => "ReviewQ could not verify the active Claude lead, turn, or report call.",
            (409, true) => {
                "The report handoff conflicts with the recorded native call or report state."
            }
            (404, false) => "This Claude session has no verified ReviewQ binding.",
            (409, false) => "Another ReviewQ companion owns this Claude session.",
            (412, _) => "Reload the ReviewQ plugin in this Claude session.",
            _ => "ReviewQ could not record this Claude session observation.",
        };
        return Err(detail.into());
    }
    println!("{}", String::from_utf8(body)?);
    Ok(())
}

#[derive(serde::Deserialize)]
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
}

pub(crate) async fn claude_hook(profile: &Path) -> Result<(), Failure> {
    let Some(bound) = std::env::var_os("REVIEWQ_CLAUDE_LINKED") else {
        return Ok(());
    };
    let mut bytes = Vec::new();
    const LIMIT: u64 = 16_384;
    std::io::stdin()
        .lock()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Ok(());
    }
    let hook: ClassicInput = serde_json::from_slice(&bytes)?;
    if bound != hook.session_id.as_str() || hook.agent_id.is_some() {
        return Ok(());
    }
    let session: NativeSessionId = hook.session_id.parse()?;
    let event = match hook.hook_event_name.as_str() {
        "PermissionRequest" if hook.tool_name.as_deref() != Some("AskUserQuestion") => {
            serde_json::json!({"kind": "permission_pending"})
        }
        "Notification" if hook.notification_type.as_deref() == Some("permission_prompt") => {
            serde_json::json!({"kind": "permission_pending"})
        }
        "MessageDisplay" if hook.index == Some(0) || hook.final_chunk == Some(true) => {
            serde_json::json!({
                "kind": "display",
                "turnId": hook.turn_id.ok_or("Missing native turn.")?,
                "messageId": hook.message_id.ok_or("Missing native display message.")?,
                "index": hook.index.ok_or("Missing native display sequence.")?,
                "finalChunk": hook.final_chunk.unwrap_or(false),
            })
        }
        "Stop" => serde_json::json!({"kind": "stopping"}),
        _ => return Ok(()),
    };
    let body = serde_json::to_vec(&serde_json::json!({
        "context": {
            "protocolVersion": 1, "sessionId": session.as_str(), "cwd": hook.cwd.canonicalize()?,
            "profile": runtime::claude_home()?.canonicalize()?, "transcript": hook.transcript_path,
        },
        "classic": event,
    }))?;
    let endpoint = runtime::read_native_endpoint(profile)?;
    let client = client()?;
    let status = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &status)?;
    let _ = bridge(&client, &endpoint, "/api/v1/native/claude/hook", body).await?;
    Ok(())
}
