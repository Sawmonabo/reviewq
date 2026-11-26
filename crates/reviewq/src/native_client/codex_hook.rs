//! Carry a Codex `Stop` or post-compaction `SessionStart` hook to ReviewQ and print its answer.

use std::path::Path;

use reviewq_protocol::ledger::{GateRequest, ReadbackRequest};
use reviewq_protocol::{HistoryTurnIdDto, ResourceIdDto};
use serde::Deserialize;

use super::hook::{Answer, GateHost, INPUT_LIMIT, forward};
use super::{Failure, read_input};

/// The fields ReviewQ reads from a Codex hook input; Codex sends more.
#[derive(Deserialize)]
struct Input {
    session_id: String,
    hook_event_name: String,
    turn_id: Option<String>,
    #[serde(default)]
    stop_hook_active: bool,
    source: Option<String>,
}

/// Sends `Stop` to the gate and a `SessionStart` whose source is `compact` to the read-back,
/// one request each, and prints the answer through `hook::forward`. Any other event is not
/// forwarded. Fails on unreadable input or when ReviewQ does not answer; the caller ignores
/// the failure so the Codex session is never broken by this hook.
pub(crate) async fn codex_hook(profile: &Path) -> Result<(), Failure> {
    let input: Input = serde_json::from_slice(&read_input(INPUT_LIMIT)?)?;
    let thread_id = ResourceIdDto::new(input.session_id).ok_or("Invalid Codex thread.")?;
    let (path, body, answer) = match (input.hook_event_name.as_str(), input.source.as_deref()) {
        ("Stop", _) => (
            "/api/v1/native/ledger/gate",
            serde_json::to_vec(&GateRequest {
                thread_id,
                turn_id: input.turn_id.and_then(HistoryTurnIdDto::new),
                stop_hook_active: input.stop_hook_active,
            })?,
            Answer::Gate(GateHost::Codex),
        ),
        ("SessionStart", Some("compact")) => (
            "/api/v1/native/ledger/readback",
            serde_json::to_vec(&ReadbackRequest { thread_id })?,
            Answer::Readback,
        ),
        _ => return Ok(()),
    };
    forward(profile, path, body, Some(answer)).await
}
