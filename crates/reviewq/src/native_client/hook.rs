//! What the Claude and Codex hook helpers share: one bounded request to ReviewQ and the one
//! place that turns its answer into what the host acts on.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use reviewq_protocol::ledger::{GateReply, ReadbackReply};

use super::{Envelope, Failure, bounded, bridge, client_with_timeout};
use crate::runtime;

/// One request, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// The status check and the hook request together. Both hosts stop a hook after 5 seconds.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(3);
/// Largest hook input read. A `Stop` input can carry the turn's whole last message, and a
/// larger input is not forwarded.
pub(super) const INPUT_LIMIT: usize = 4 * 1024 * 1024;
/// Largest reply: a read-back of at most 4,000 characters inside the envelope.
const REPLY_LIMIT: usize = 64 * 1024;

/// The answer a forwarded hook expects from ReviewQ.
#[derive(Clone, Copy)]
pub(super) enum Answer {
    /// A `Stop` hook: a `GateReply` that may keep the turn open, in the host's form.
    Gate(GateHost),
    /// A `SessionStart` hook after compaction: a `ReadbackReply` for the model's context.
    Readback,
}

/// Posts `body` to `path` once, after the signed status check `bridge` makes, and, when
/// `answer` is set, prints what the reply asks the host to do. Fails when the service is not
/// running, the listener is not this installation's ready service, either request takes
/// longer than `REQUEST_TIMEOUT` or both together longer than `TOTAL_TIMEOUT`, or ReviewQ
/// refuses the request or answers in an unexpected shape; the caller ignores the failure, so
/// the hook then prints nothing.
pub(super) async fn forward(
    profile: &Path,
    path: &str,
    body: Vec<u8>,
    answer: Option<Answer>,
) -> Result<(), Failure> {
    let endpoint = runtime::read_native_endpoint(profile)?;
    let client = client_with_timeout(Some(REQUEST_TIMEOUT))?;
    let bytes = tokio::time::timeout(TOTAL_TIMEOUT, async {
        let response = bridge(&client, &endpoint, path, body).await?;
        if !response.status().is_success() {
            return Err("ReviewQ refused the hook.".into());
        }
        bounded(response, REPLY_LIMIT).await
    })
    .await
    .map_err(|_| "ReviewQ did not answer the hook in time.")??;
    if let Some(output) = answer
        .map(|answer| answer.output(&bytes))
        .transpose()?
        .flatten()
    {
        // A host that closed stdout gets nothing; the hook still exits 0.
        let _ = writeln!(std::io::stdout().lock(), "{output}");
    }
    Ok(())
}

/// The host whose `Stop` hook the gate answers. Each keeps the turn open in its own form.
#[derive(Clone, Copy)]
pub(super) enum GateHost {
    /// `{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"…"}}`, which
    /// Claude Code 2.1.163 and later shows as hook feedback. `{"decision":"block"}` works
    /// there too, but is shown to the user as a hook error on every block.
    Claude,
    /// `{"decision":"block","reason":"…"}`.
    Codex,
}

impl Answer {
    /// What the host acts on, as one line: for a block with a reason, the host's form of it;
    /// for a read-back, its text. `None` when the reply allows the turn to end or carries no
    /// text. Fails when `reply` is not the expected envelope.
    fn output(self, reply: &[u8]) -> Result<Option<String>, Failure> {
        Ok(match self {
            Self::Gate(host) => match serde_json::from_slice::<Envelope<GateReply>>(reply)?.data {
                GateReply {
                    block: true,
                    reason: Some(reason),
                } if !reason.is_empty() => Some(
                    match host {
                        GateHost::Claude => serde_json::json!({"hookSpecificOutput": {
                            "hookEventName": "Stop", "additionalContext": reason,
                        }}),
                        GateHost::Codex => {
                            serde_json::json!({"decision": "block", "reason": reason})
                        }
                    }
                    .to_string(),
                ),
                GateReply { .. } => None,
            },
            Self::Readback => {
                let ReadbackReply { text } =
                    serde_json::from_slice::<Envelope<ReadbackReply>>(reply)?.data;
                (!text.is_empty()).then_some(text)
            }
        })
    }
}
