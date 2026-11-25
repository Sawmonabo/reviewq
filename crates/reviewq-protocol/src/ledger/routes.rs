//! Bodies of the native ledger routes under `/api/v1/native/ledger/`. The tool routes serve the
//! Claude plugin's bridge, and the gate and read-back routes serve the Codex hooks.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{HistoryTurnIdDto, ResourceIdDto, SessionKeyDto};

/// The caller the Claude plugin proves for one tool call. It never travels inside `input`, and
/// no tool schema shows it to the model. Codex calls the same tools through the MCP server and
/// never sends it.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerBinding {
    /// ReviewQ's key for the Claude session the plugin linked.
    pub session_key: SessionKeyDto,
    /// Claude's tool-use ID for this call. A repeat of the same ID returns the stored result.
    pub call_id: ResourceIdDto,
    /// The Claude turn the call runs in, when the plugin knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<HistoryTurnIdDto>,
    /// The calling subagent's ID, absent when the session's own agent calls.
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

/// The `gate` route's body, sent by the Codex `Stop` hook.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateRequest {
    /// The Codex thread the hook input's `session_id` names.
    pub thread_id: ResourceIdDto,
    /// The Codex turn that is ending, when the hook input names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<HistoryTurnIdDto>,
    /// The hook input's `stop_hook_active`: true when this stop follows a block.
    pub stop_hook_active: bool,
}

/// The `gate` reply: `{block:false}`, or `{block:true,reason}` with what the turn still owes.
/// Claude's existing hook route answers its `Stop` hook with the same reply.
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

/// The `readback` route's body, sent by the Codex `SessionStart` hook after Codex compacts the
/// lead's conversation.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadbackRequest {
    /// The Codex thread the hook input's `session_id` names.
    pub thread_id: ResourceIdDto,
}

/// The `readback` reply: the rule and the short ledger list, as text for the model. Claude's
/// existing hook route answers its `SessionStart` hook with the same reply.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadbackReply {
    pub text: String,
}
