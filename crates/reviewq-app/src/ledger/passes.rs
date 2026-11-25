//! `needs_input`, which holds the open pass, and the stop gate, whose block reason names what
//! the pass owes and how many findings are on file.

use reviewq_domain::ledger::{GateDecision, PassKind};

use super::{CallBinding, GateAnswer, GateRequest, HoldReceipt, LedgerStore, OpenPass};
use crate::access::{AppError, FieldError};

/// Holds the run's open pass with `reason`, the question the reviewer must answer, until the
/// lead's next turn starts. Completes no assessment and lets the turn end.
///
/// Fails with a field error when no pass is open, or when the call does not come from the
/// run's lead: from a child, or from a session the store attributes to no run. Other store
/// errors pass through.
pub async fn needs_input(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    reason: String,
) -> Result<HoldReceipt, AppError> {
    match store.hold_pass(binding, reason).await {
        Ok(Some(receipt)) => Ok(receipt),
        Ok(None) => Err(AppError::InvalidFields(vec![FieldError::new(
            "",
            "Nothing is waiting to be closed, so there is nothing to hold. Ask your question in your reply instead.",
        )])),
        Err(AppError::Forbidden) => Err(AppError::InvalidFields(vec![FieldError::new(
            "",
            "Only the review's lead can call needs_input, and this call does not come from it. Subagents file findings and notes and leave closing the review to the lead.",
        )])),
        Err(error) => Err(error),
    }
}

/// Answers a host's stop hook. The store applies the domain's `gate` to its stored facts and
/// records the block count. Returns the block reason the host shows the model, or `None` to
/// let the turn end.
pub async fn gate(
    store: &(impl LedgerStore + ?Sized),
    request: GateRequest,
) -> Result<Option<String>, AppError> {
    let answer = store.answer_gate(request).await?;
    Ok(block_reason(&answer))
}

/// The reason for a block, in under 600 characters: what the closing call needs and how many
/// findings are on file. `None` for a release.
fn block_reason(answer: &GateAnswer) -> Option<String> {
    let GateDecision::Block { .. } = answer.decision else {
        return None;
    };
    let on_file = match answer.active_findings {
        0 => "no finding is on file yet".to_owned(),
        1 => "1 finding is on file".to_owned(),
        count => format!("{count} findings are on file"),
    };
    Some(format!(
        "This review is not closed yet, and {on_file}. Before you stop, {}, or call needs_input if the review must wait for the reviewer's answer. The reviewer sees nothing you filed until then.",
        closing_call(answer.pass.as_ref()),
    ))
}

/// The closing call `pass` owes, as an instruction: `call finish_review with …`. A full
/// review needs a verdict and the reviewed commit; a selected-thread check needs one
/// assessment per selected discussion. Shared by the gate and the read-back.
pub(super) fn closing_call(pass: Option<&OpenPass>) -> String {
    let Some(pass) = pass else {
        return "call finish_review".to_owned();
    };
    match pass.kind {
        PassKind::FullReview => {
            "call finish_review with your verdict and the reviewed commit".to_owned()
        }
        PassKind::SelectedThreads => {
            match pass.batch.as_ref().map(|batch| batch.discussion_ids.len()) {
                Some(1) => {
                    "call finish_review with one assessment for the selected discussion".to_owned()
                }
                Some(count) => format!(
                    "call finish_review with one assessment for each of the {count} selected discussions"
                ),
                None => {
                    "call finish_review with one assessment for each selected discussion".to_owned()
                }
            }
        }
    }
}
