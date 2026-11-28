//! `wait_for_user`, which holds the open pass, and the stop gate, whose block reason names what
//! the pass owes and how many findings are saved.

use std::fmt::Write as _;

use reviewq_domain::ledger::{GateDecision, PassKind, PassState, Tool};

use super::readback::push_quoted;
use super::record::capitalized;
use super::refusal::{for_agent, root};
use super::required;
use super::{CallBinding, GateAnswer, GateRequest, HoldReceipt, LedgerStore, OpenPass};
use crate::access::AppError;

/// Holds the run's open pass with `question`, the question the user must answer, until the
/// lead's next turn starts. Completes no assessment and lets the turn end.
///
/// Fails with a root field error when no pass is open, or when the call does not come from
/// the run's lead: from a child, or from a session the store attributes to no run. Other store
/// errors pass through.
pub async fn wait_for_user(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    question: String,
) -> Result<HoldReceipt, AppError> {
    let session = binding.session.clone();
    match store.hold_pass(binding, question).await {
        Ok(Some(receipt)) => Ok(receipt),
        Ok(None) => Err(root(
            "No review is open, so there is nothing to pause. Ask your question in your reply instead.",
        )),
        Err(error) => Err(for_agent(store, &session, Tool::WaitForUser, error).await),
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

/// The reason for a block, in under 600 characters plus a question the lead asked before the
/// pass reopened: how many findings are saved, what each of the two calls does, and that one
/// of them is owed before the turn ends. `None` for a release.
fn block_reason(answer: &GateAnswer) -> Option<String> {
    let GateDecision::Block { .. } = answer.decision else {
        return None;
    };
    let owed = answer.pass.as_ref().map_or_else(
        || {
            format!(
                "Call {}, which shows the user what is saved, or {} if the review must wait for the user's reply.",
                Tool::SubmitReview.as_str(),
                Tool::WaitForUser.as_str()
            )
        },
        owed,
    );
    Some(format!(
        "This review is not submitted yet, and {}. {owed} Make one of these calls before you stop.",
        findings_saved(answer.active_findings)
    ))
}

/// `no finding is saved yet`, `1 finding is saved` or `{count} findings are saved`.
pub(super) fn findings_saved(count: u32) -> String {
    match count {
        0 => "no finding is saved yet".to_owned(),
        1 => "1 finding is saved".to_owned(),
        count => format!("{count} findings are saved"),
    }
}

/// What `pass` owes, for an agent. While it waits: the question the lead asked, quoted as data
/// on one line, and the closing call once the user replies. Once the lead's next turn reopened
/// it after a hold: that question again, the closing call if the lead has the answer and
/// `wait_for_user` again if it still waits. While it is open and was never held: the closing
/// call with what it needs and that only it publishes, and `wait_for_user` for a review that
/// must wait on the user, which publishes nothing. The gate, the read-back and `read_draft`'s
/// open pass all state it in these words.
pub fn owed(pass: &OpenPass) -> String {
    let closing = closing_call(pass);
    let wait = Tool::WaitForUser.as_str();
    match (pass.state, &pass.question) {
        (PassState::Waiting, Some(question)) => {
            let mut sentence = asked(question, "");
            let _ = write!(sentence, ". Once the user replies, {closing}.");
            sentence
        }
        (PassState::Waiting, None) => {
            format!("Once the user replies to your question, {closing}.")
        }
        (PassState::Open | PassState::Closed, Some(question)) => {
            let mut sentence = asked(question, " earlier");
            let _ = write!(
                sentence,
                ". If you now have the answer, {closing}: only that call shows the user what is saved. If the review still waits for the answer, call {wait} again with that question and ask it in your reply as well."
            );
            sentence
        }
        (PassState::Open | PassState::Closed, None) => format!(
            "{}: only that call shows the user what is saved. If the review must wait for the user's answer to a question, call {wait} with that question instead and ask it in your reply as well; {wait} publishes nothing and only records that the review is waiting.",
            capitalized(&closing)
        ),
    }
}

/// `You asked the user this{when} with wait_for_user, quoted as you wrote it: "…"`.
fn asked(question: &str, when: &str) -> String {
    let mut sentence = format!(
        "You asked the user this{when} with {}, quoted as you wrote it: ",
        Tool::WaitForUser.as_str()
    );
    // The tool input already bounds the question, so it is quoted whole.
    push_quoted(&mut sentence, question, usize::MAX);
    sentence
}

/// The closing call `pass` owes, as an instruction: `call submit_review with …`. A full
/// review needs a verdict, the reviewed commit and the required sections saved; a
/// selected-thread check needs one assessment per selected discussion.
fn closing_call(pass: &OpenPass) -> String {
    let submit = Tool::SubmitReview.as_str();
    match pass.kind {
        PassKind::FullReview => format!(
            "call {submit} with your verdict and the reviewed commit, with {} saved",
            required::named(&required::REQUIRED_NOTES).unwrap_or_default()
        ),
        PassKind::SelectedThreads => {
            match pass.batch.as_ref().map(|batch| batch.discussion_ids.len()) {
                Some(1) => {
                    format!("call {submit} with one assessment for the selected discussion")
                }
                Some(count) => format!(
                    "call {submit} with one assessment for each of the {count} selected discussions"
                ),
                None => format!("call {submit} with one assessment for each selected discussion"),
            }
        }
    }
}
