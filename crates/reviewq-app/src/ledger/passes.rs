//! `needs_input`, which holds the open pass, and the stop gate, whose block reason names what
//! the pass owes and how many findings are on file.

use std::fmt::Write as _;

use reviewq_domain::ledger::{GateDecision, PassKind, PassState, Tool};

use super::readback::push_quoted;
use super::record::capitalized;
use super::refusal::{for_agent, root};
use super::{CallBinding, GateAnswer, GateRequest, HoldReceipt, LedgerStore, OpenPass};
use crate::access::AppError;

/// Holds the run's open pass with `reason`, the question the reviewer must answer, until the
/// lead's next turn starts. Completes no assessment and lets the turn end.
///
/// Fails with a root field error when no pass is open, or when the call does not come from
/// the run's lead: from a child, or from a session the store attributes to no run. Other store
/// errors pass through.
pub async fn needs_input(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    reason: String,
) -> Result<HoldReceipt, AppError> {
    let session = binding.session.clone();
    match store.hold_pass(binding, reason).await {
        Ok(Some(receipt)) => Ok(receipt),
        Ok(None) => Err(root(
            "Nothing is waiting to be closed, so there is nothing to hold. Ask your question in your reply instead.",
        )),
        Err(error) => Err(for_agent(store, &session, Tool::NeedsInput, error).await),
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

/// The reason for a block, in under 600 characters: how many findings are on file, what each
/// of the two calls does, and that one of them is owed before the turn ends. `None` for a
/// release.
fn block_reason(answer: &GateAnswer) -> Option<String> {
    let GateDecision::Block { .. } = answer.decision else {
        return None;
    };
    let owed = answer.pass.as_ref().map_or_else(
        || "Call finish_review, which shows the reviewer what is on file, or needs_input if the review must wait for the reviewer's answer.".to_owned(),
        owed,
    );
    Some(format!(
        "This review is not closed yet, and {}. {owed} Make one of these calls before you stop.",
        findings_on_file(answer.active_findings)
    ))
}

/// `no finding is on file yet`, `1 finding is on file` or `{count} findings are on file`.
pub(super) fn findings_on_file(count: u32) -> String {
    match count {
        0 => "no finding is on file yet".to_owned(),
        1 => "1 finding is on file".to_owned(),
        count => format!("{count} findings are on file"),
    }
}

/// What `pass` owes, for an agent. While it is open: the closing call with what it needs and
/// that only it publishes, and `needs_input` for a review that must wait on the reviewer,
/// which publishes nothing. While it waits: the question the lead asked, quoted as data on one
/// line, and the closing call once the reviewer answers. The gate, the read-back and
/// `review_ledger`'s open pass all state it in these words.
pub fn owed(pass: &OpenPass) -> String {
    let closing = closing_call(pass);
    match pass.state {
        PassState::Waiting => match &pass.question {
            Some(question) => {
                let mut sentence =
                    "You asked the reviewer this with needs_input, quoted as you wrote it: "
                        .to_owned();
                // The tool input already bounds the question, so it is quoted whole.
                push_quoted(&mut sentence, question, usize::MAX);
                let _ = write!(sentence, ". Once the reviewer answers, {closing}.");
                sentence
            }
            None => format!("Once the reviewer answers your question, {closing}."),
        },
        PassState::Open | PassState::Closed => format!(
            "{}: only that call shows the reviewer what is on file. If the review must wait for the reviewer's answer to a question, call needs_input with that question instead and ask it in your reply as well; needs_input publishes nothing and only records that the review is waiting.",
            capitalized(&closing)
        ),
    }
}

/// The closing call `pass` owes, as an instruction: `call finish_review with …`. A full
/// review needs a verdict and the reviewed commit; a selected-thread check needs one
/// assessment per selected discussion.
fn closing_call(pass: &OpenPass) -> String {
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
