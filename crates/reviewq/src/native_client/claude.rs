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
    fields.insert(
        "context".into(),
        serde_json::json!({
            "protocolVersion": 1,
            "sessionId": native.as_str(),
            "cwd": std::env::current_dir()?.canonicalize()?,
            "profile": runtime::claude_home()?.canonicalize()?,
        }),
    );
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
