//! A refused reviewing tool call as a sentence for the calling agent: one the store could not bind to
//! a review, and a lead-only call from a subagent. Each becomes the call's one root field
//! error, which the agent reads and acts on in its turn.

use reviewq_domain::ledger::{CallerRole, Tool, may_call};

use super::{HostSession, LedgerRead, LedgerSelection, LedgerStore};
use crate::access::{AppError, FieldError};

/// `error` from a call of `tool` made as `session`, with `Forbidden` turned into a root field
/// error saying what to do next; every other error is returned unchanged. The store refuses
/// both an unbound caller and a subagent's lead-only call with `Forbidden`. For a lead-only
/// tool the two are told apart by the binding when it names a Claude subagent, and otherwise
/// by reading the short list of saved findings and sections as the caller, which a subagent may do and an unbound caller
/// may not.
pub(super) async fn for_agent(
    store: &(impl LedgerStore + ?Sized),
    session: &HostSession,
    tool: Tool,
    error: AppError,
) -> AppError {
    if error != AppError::Forbidden {
        return error;
    }
    let lead_only = !may_call(tool, CallerRole::Child);
    let message = if lead_only && from_subagent(store, session).await {
        format!(
            "Only the review's lead agent can call {}, and this call comes from a subagent. Report what you found to the lead agent instead; you can still save findings and sections with {}, {} and {}.",
            tool.as_str(),
            Tool::SaveFinding.as_str(),
            Tool::SetFindingOutcome.as_str(),
            Tool::SaveSection.as_str()
        )
    } else {
        unlinked(session, tool)
    };
    AppError::InvalidFields(vec![FieldError::new("", message)])
}

/// A root field error with `message`, for a refusal the caller words itself.
pub(super) fn root(message: &str) -> AppError {
    AppError::InvalidFields(vec![FieldError::new("", message)])
}

/// Whether a `Forbidden` lead-only call came from a verified subagent of a review's lead.
async fn from_subagent(store: &(impl LedgerStore + ?Sized), session: &HostSession) -> bool {
    match session {
        HostSession::Claude { agent, .. } => agent.is_some(),
        HostSession::Codex { .. } => {
            let read = LedgerRead {
                selection: LedgerSelection::Short,
                after: None,
            };
            store.read_ledger(session.clone(), read).await.is_ok()
        }
    }
}

/// What a caller the store could not bind to any review does next, with the link command
/// the user types in that host.
fn unlinked(session: &HostSession, tool: Tool) -> String {
    let tool = tool.as_str();
    match session {
        HostSession::Claude { .. } => format!(
            "This conversation is not linked to a ReviewQ review, so ReviewQ did nothing with this call. Ask the user to link it by running /reviewq <merge request or ticket> in this conversation, then call {tool} again."
        ),
        HostSession::Codex { .. } => format!(
            "ReviewQ cannot tie this conversation to a review: it is not linked to one, and it is not a subagent of a linked lead that ReviewQ knows, so ReviewQ did nothing with this call. If this is the lead's conversation, ask the user to link it by running $reviewq <merge request or ticket> in it, then call {tool} again. If you are a subagent, report what you found to the lead agent instead."
        ),
    }
}
