//! Bodies of the native ledger routes under `/api/v1/native/ledger/`. The plugin, the bridge, the
//! hook helpers and the routes all use these.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{HistoryTurnIdDto, ResourceIdDto, SessionKeyDto};

/// The caller a host proves for one tool call. It never travels inside `input`, and no tool
/// schema shows it to the model.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerBinding {
    /// The native session the call comes from: Claude's linked session, or the Codex thread
    /// that calls, or that thread's parent when the request names one.
    pub session_key: SessionKeyDto,
    /// The host's ID for this call: Claude's tool-use ID or Codex's MCP call ID. A repeat of the
    /// same ID returns the stored result.
    pub call_id: ResourceIdDto,
    /// The host turn the call runs in, when the host names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<HistoryTurnIdDto>,
    /// The calling subagent, absent when the session's own agent calls: Claude's subagent ID,
    /// or a Codex child's own thread when its request names its parent thread as `sessionKey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<ResourceIdDto>,
}

/// A tool route's body: `{binding,input}`. `input` stays raw JSON so that
/// `validate_ledger_input` reports every refused field instead of one decode error.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerRequest {
    pub binding: LedgerBinding,
    pub input: Value,
}

/// The `gate` route's body, sent by a host's stop hook.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateRequest {
    pub session_key: SessionKeyDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<HistoryTurnIdDto>,
    /// The hook input's `stop_hook_active`: true when this stop follows a block.
    pub stop_hook_active: bool,
}

/// The `gate` reply: `{block:false}`, or `{block:true,reason}` with what the turn still owes.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GateReply {
    pub block: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl GateReply {
    /// Lets the turn end.
    pub fn allow() -> Self {
        Self {
            block: false,
            reason: None,
        }
    }

    /// Keeps the turn open with `reason`, which the host shows the model.
    pub fn block(reason: impl Into<String>) -> Self {
        Self {
            block: true,
            reason: Some(reason.into()),
        }
    }
}

/// The `readback` route's body, sent after the host compacts the lead's conversation.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadbackRequest {
    pub session_key: SessionKeyDto,
}

/// The `readback` reply: the rule and the short ledger list, as text for the model.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadbackReply {
    pub text: String,
}
