use std::io::Read;
use std::path::Path;

use reviewq_domain::identity::NativeSessionId;

use super::{Failure, bounded, bridge, client, signed, verify_ready};
use crate::runtime;

#[derive(Clone, clap::ValueEnum)]
pub(crate) enum ClaudeBridgeOperation {
    Heartbeat,
    DeliveryCheck,
    Hook,
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
    let fields = payload
        .as_object_mut()
        .ok_or("Invalid ReviewQ companion payload.")?;
    if fields.contains_key("context") {
        return Err("ReviewQ companion context is supplied by the native client.".into());
    }
    match operation {
        ClaudeBridgeOperation::Report | ClaudeBridgeOperation::ReportStatus => {
            let turn = fields
                .remove("turnId")
                .ok_or("Missing native Claude turn.")?;
            let call = fields
                .remove("callId")
                .ok_or("Missing native Claude call.")?;
            fields.insert(
                "context".into(),
                serde_json::json!({
                    "sessionKey": format!("claude:{native}"), "turnId": turn, "callId": call
                }),
            );
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
    let endpoint = runtime::read_endpoint(profile)?;
    let client = client()?;
    let status = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &status)?;
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

#[derive(serde::Deserialize)]
struct ClassicInput {
    session_id: String,
    transcript_path: Option<std::path::PathBuf>,
    cwd: std::path::PathBuf,
    hook_event_name: String,
    agent_id: Option<String>,
    notification_type: Option<String>,
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
        "PermissionRequest" => serde_json::json!({"kind": "permission_pending"}),
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
    let endpoint = runtime::read_endpoint(profile)?;
    let client = client()?;
    let status = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &status)?;
    let _ = bridge(&client, &endpoint, "/api/v1/native/claude/hook", body).await?;
    Ok(())
}
