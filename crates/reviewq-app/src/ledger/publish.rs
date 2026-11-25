//! Publishing: `finish_review`'s closing checks, composing outside any transaction, the
//! conditional commit with at most three recomposes on a stale snapshot and the display index
//! after it; and settling, which publishes an update or records `unfinished_review` or
//! `update_failed` from the domain's `settle` decision.

use std::cmp::Reverse;
use std::fmt::{self, Display, Formatter};

use reviewq_domain::identity::{GitSha, ReceiptId, ReportHash, Revision, RunId};
use reviewq_domain::ledger::{
    ClosingError, ClosingFacts, FailureKind, Settlement, Tool, VersionKind, VersionState,
    closing_kind, settle as settlement, untouched_findings,
};

use super::compose::{self, checked_revision, finding_bytes, recorded_outcome, state};
use super::refusal::{for_agent, root};
use super::{
    CallBinding, ClosingCommit, ClosingRead, ClosingSnapshot, ClosingSummary, CommitKind,
    FailureRecord, FinishReceipt, FinishReview, LedgerSnapshot, LedgerStore, NewVersion,
    NoteSection, PublishCommit, PublishOutcome, SettleCandidate, VersionFacts,
};
use crate::access::{AppError, FieldError};
use crate::followups::ResultKind;
use crate::reports::{
    Coverage, FindingEdit, ReportEdits, ReportSummary, RevisionEvidence, Verdict,
};

/// Recomposes after a stale snapshot before a publish gives up.
const MAX_RECOMPOSES: usize = 3;
/// Largest composed report text, in UTF-8 bytes.
const MAX_COMPOSED_BYTES: usize = 4 * 1024 * 1024;
/// Records a refusal for size names, largest first.
const NAMED_RECORDS: usize = 3;
/// The notes whose absence the closing acknowledgement reports, in its order.
const REPORTED_NOTES: [NoteSection; 5] = [
    NoteSection::Summary,
    NoteSection::Rationale,
    NoteSection::Flow,
    NoteSection::Changes,
    NoteSection::Verification,
];

const CLOSING_REFUSED: &str = "ReviewQ refused the lead's closing call because this run's workspace, repository grant, settings or follow-up batch changed, or the run's lead changed. The filed findings are kept. Check the run, then ask the lead to call finish_review again.";
const UPDATE_REFUSED: &str = "ReviewQ could not publish an update because this run's workspace, repository grant, settings or lead changed. The filed findings are kept, and ReviewQ tries again the next time the run settles.";
const UPDATE_KEPT_CHANGING: &str = "ReviewQ could not publish an update because the findings kept changing while it was composed. They are kept, and ReviewQ tries again the next time the run settles.";
/// What the lead reads when its closing call is refused; the user reads `CLOSING_REFUSED`.
const CLOSING_REFUSED_FOR_AGENT: &str = "ReviewQ could not close this review because its workspace, repository access, settings or follow-up batch changed, or the review's lead changed. Every finding and note is kept, and ReviewQ already shows the user why. Stop here and tell the user the review could not be closed; do not call finish_review again until they have looked.";
/// What the lead reads when the records kept changing through every recompose.
const KEPT_CHANGING_FOR_AGENT: &str = "Findings or notes kept changing while ReviewQ composed the report, so nothing was published yet, and every record is kept. If subagents are still filing, wait for them to finish, then call finish_review again.";

/// Closes the run's review for the lead and publishes, from a snapshot read in one short
/// transaction and committed conditionally; nothing is held open while composing. A repeat of
/// an answered call, or a new call that repeats the lead's last closing call with nothing
/// changed, returns that acknowledgement. A call that would only repeat the current version as
/// an update returns `unchanged` with that version and writes nothing. After a publish it
/// builds the display index.
///
/// Fails with `InvalidFields` for anything the lead corrects in its turn: a missing verdict
/// or revision, `fullReview` or assessments that do not fit the open pass, a revision other
/// than the selected-thread batch's, findings a full review left untouched, assessments that
/// do not cover the batch, or text over 4 MiB. Every other refusal is also one root field
/// error worded for the agent: after recording `closing_refused` when the run's workspace,
/// grant, settings, follow-up binding or lead changed; for a child or an unattributed call,
/// which records nothing; and when the records kept changing through three recomposes. Calls
/// for different runs may run at once; a call that loses a race on its own run recomposes.
pub async fn finish_review(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    input: FinishReview,
) -> Result<FinishReceipt, AppError> {
    let session = binding.session.clone();
    match close(store, binding, &input).await {
        Err(error) => Err(for_agent(store, &session, Tool::FinishReview, error).await),
        closed => closed,
    }
}

/// `finish_review` up to its refusals for a caller the store cannot bind, which it returns
/// as the store's `Forbidden`.
async fn close(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    input: &FinishReview,
) -> Result<FinishReceipt, AppError> {
    for _ in 0..=MAX_RECOMPOSES {
        let closing = match store.closing_snapshot(binding.clone()).await? {
            ClosingRead::Replayed(ack) => return Ok(ack),
            ClosingRead::Refused(refused) => {
                store
                    .record_failure(FailureRecord {
                        run: refused.run,
                        pass: refused.pass,
                        kind: FailureKind::ClosingRefused,
                        reason: CLOSING_REFUSED.to_owned(),
                        sequence: refused.sequence,
                        turn: refused.turn,
                    })
                    .await?;
                return Err(root(CLOSING_REFUSED_FOR_AGENT));
            }
            ClosingRead::Current(closing) => *closing,
        };
        if let Some(ack) = repeated(&closing, input).or_else(|| restates_current(&closing, input)) {
            return Ok(ack);
        }
        let prepared = prepare_closing(closing, input)?;
        match store.commit_publish(prepared.commit).await {
            Ok(PublishOutcome::Published(receipt)) => {
                // The version is readable without its index, so a failure here is ignored.
                let _ = store.index_version(prepared.run, receipt.clone()).await;
                return Ok(FinishReceipt {
                    receipt,
                    sha256: prepared.sha256,
                    summary: prepared.summary,
                });
            }
            Ok(PublishOutcome::Unchanged) => {
                return Ok(FinishReceipt {
                    receipt: prepared.current.ok_or(AppError::Internal)?,
                    sha256: prepared.sha256,
                    summary: prepared.summary,
                });
            }
            Ok(PublishOutcome::Stale) => {}
            Ok(PublishOutcome::Invalid(errors)) => return Err(AppError::InvalidFields(errors)),
            Err(AppError::Conflict) => {
                store.record_failure(prepared.refusal).await?;
                return Err(root(CLOSING_REFUSED_FOR_AGENT));
            }
            Err(error) => return Err(error),
        }
    }
    Err(root(KEPT_CHANGING_FOR_AGENT))
}

/// Settles every run that may need it from its stored state: publishes the update a run owes,
/// or records `unfinished_review` for an open pass its idle lead left, or does nothing. Runs at
/// startup and on the periodic tick. A run already handled costs one read and no write, so
/// calls may overlap. One run's error does not stop the others; the first is returned after
/// all were tried.
pub async fn settle_due(store: &(impl LedgerStore + ?Sized)) -> Result<(), AppError> {
    let mut first = None;
    for candidate in store.runs_to_settle().await? {
        if let Err(error) = settle_run(store, candidate).await {
            first.get_or_insert(error);
        }
    }
    first.map_or(Ok(()), Err)
}

async fn settle_run(
    store: &(impl LedgerStore + ?Sized),
    candidate: SettleCandidate,
) -> Result<(), AppError> {
    match settlement(candidate.facts) {
        Settlement::Nothing => Ok(()),
        Settlement::Unfinished => {
            store
                .record_failure(FailureRecord {
                    reason: unfinished_reason(candidate.active_findings),
                    run: candidate.run,
                    pass: candidate.pass,
                    kind: FailureKind::UnfinishedReview,
                    sequence: candidate.sequence,
                    turn: None,
                })
                .await
        }
        Settlement::PublishUpdate => publish_update(store, candidate.run, candidate.sequence).await,
    }
}

/// Publishes the update a settled run owes. A snapshot that shows an open pass or nothing
/// unpublished means another trigger or the lead got there first, and nothing is written. An
/// update that cannot publish records `update_failed`.
async fn publish_update(
    store: &(impl LedgerStore + ?Sized),
    run: RunId,
    mut sequence: Revision,
) -> Result<(), AppError> {
    for _ in 0..=MAX_RECOMPOSES {
        let snapshot = match store.update_snapshot(run.clone()).await {
            Ok(snapshot) => snapshot,
            Err(AppError::Conflict) => {
                return update_failed(store, run, sequence, UPDATE_REFUSED.to_owned()).await;
            }
            Err(error) => return Err(error),
        };
        sequence = snapshot.guard.sequence;
        let Some(parent) = &snapshot.parent else {
            return Ok(());
        };
        if snapshot.pass.is_some() || parent.sequence == snapshot.guard.sequence {
            return Ok(());
        }
        let facts = update_facts(&snapshot, None, None)?;
        let version = match new_version(&snapshot, facts) {
            Ok(version) => version,
            Err(too_large) => {
                return update_failed(store, run, sequence, too_large.to_string()).await;
            }
        };
        let commit = PublishCommit {
            guard: snapshot.guard,
            kind: CommitKind::Update(Box::new(version)),
        };
        match store.commit_publish(commit).await {
            Ok(PublishOutcome::Published(receipt)) => {
                // The version is readable without its index, so a failure here is ignored.
                let _ = store.index_version(run, receipt).await;
                return Ok(());
            }
            Ok(PublishOutcome::Stale) => {}
            Ok(PublishOutcome::Unchanged | PublishOutcome::Invalid(_)) => {
                return Err(AppError::Internal);
            }
            Err(AppError::Conflict) => {
                return update_failed(store, run, sequence, UPDATE_REFUSED.to_owned()).await;
            }
            Err(error) => return Err(error),
        }
    }
    update_failed(store, run, sequence, UPDATE_KEPT_CHANGING.to_owned()).await
}

async fn update_failed(
    store: &(impl LedgerStore + ?Sized),
    run: RunId,
    sequence: Revision,
    reason: String,
) -> Result<(), AppError> {
    store
        .record_failure(FailureRecord {
            run,
            pass: None,
            kind: FailureKind::UpdateFailed,
            reason,
            sequence,
            turn: None,
        })
        .await
}

fn unfinished_reason(findings: u32) -> String {
    let on_file = match findings {
        1 => "1 finding is".to_owned(),
        count => format!("{count} findings are"),
    };
    format!(
        "The lead ended without finishing the review: it called neither finish_review nor needs_input. {on_file} on file and stay unpublished until the lead closes the review."
    )
}

/// A closing call ready to commit, with what its acknowledgement and a refusal need.
struct PreparedClosing {
    commit: PublishCommit,
    run: RunId,
    /// The composed text's digest, or the current version's for an `unchanged` result.
    sha256: ReportHash,
    /// The current version, which an `unchanged` result names.
    current: Option<ReceiptId>,
    summary: ClosingSummary,
    /// What a refused commit records.
    refusal: FailureRecord,
}

/// The last closing call's acknowledgement when this call repeats it with nothing changed:
/// no record changed since, no pass opened, its version is still current, and the call
/// states nothing other than the current version's verdict, revision and kind or the
/// assessments that call closed with.
fn repeated(closing: &ClosingSnapshot, input: &FinishReview) -> Option<FinishReceipt> {
    let last = closing.last_closing.as_ref()?;
    let snapshot = &closing.snapshot;
    let parent = snapshot.parent.as_ref()?;
    let unchanged = last.sequence == snapshot.guard.sequence
        && snapshot.pass.is_none()
        && snapshot.guard.head.as_ref() == Some(&last.ack.receipt);
    let restated = restates_version(&parent.summary, input)
        && (!input.full_review || parent.kind == VersionKind::FullReview)
        && (input.assessments.is_empty() || input.assessments == last.assessments);
    (unchanged && restated).then(|| last.ack.clone())
}

/// The `unchanged` acknowledgement, naming the current version, for a closing call that would
/// publish an update identical to it: no pass is open, no record changed since the current
/// version, and the call asks for no full review and states no verdict or revision other than
/// the current version's. Nothing is written. A first version, a full review and a pass's
/// close always commit.
fn restates_current(closing: &ClosingSnapshot, input: &FinishReview) -> Option<FinishReceipt> {
    let snapshot = &closing.snapshot;
    let parent = snapshot.parent.as_ref()?;
    let receipt = snapshot.guard.head.clone()?;
    let nothing_new = snapshot.pass.is_none()
        && !input.full_review
        && input.assessments.is_empty()
        && parent.sequence == snapshot.guard.sequence
        && restates_version(&parent.summary, input);
    nothing_new.then(|| FinishReceipt {
        receipt,
        sha256: parent.summary.reference.hash.clone(),
        summary: closing_summary(snapshot, ResultKind::Unchanged),
    })
}

/// Whether the call's verdict and revision, when given, equal the version's.
fn restates_version(summary: &ReportSummary, input: &FinishReview) -> bool {
    input
        .verdict
        .is_none_or(|verdict| verdict == summary.verdict)
        && input
            .revision
            .as_ref()
            .is_none_or(|revision| *revision == parent_reviewed(summary))
}

/// Runs the closing checks and composes the version the call publishes, or decides that a
/// selected-thread pass completes `unchanged` without one.
fn prepare_closing(
    closing: ClosingSnapshot,
    input: &FinishReview,
) -> Result<PreparedClosing, AppError> {
    let ClosingSnapshot {
        caller, snapshot, ..
    } = closing;
    let kind = closing_kind(ClosingFacts {
        has_version: snapshot.parent.is_some(),
        pass: snapshot.pass.as_ref().map(|pass| pass.kind),
        verdict: input.verdict.is_some(),
        revision: input.revision.is_some(),
        full_review: input.full_review,
        assessments: !input.assessments.is_empty(),
    })
    .map_err(|errors| AppError::InvalidFields(errors.into_iter().map(closing_error).collect()))?;
    let errors = match kind {
        VersionKind::FullReview => untouched_errors(&snapshot),
        VersionKind::SelectedThreads => revision_errors(&snapshot, input),
        VersionKind::Update => Vec::new(),
    };
    if !errors.is_empty() {
        return Err(AppError::InvalidFields(errors));
    }
    let facts = match kind {
        VersionKind::FullReview => full_review_facts(&snapshot, input)?,
        VersionKind::SelectedThreads => selected_threads_facts(&snapshot, input)?,
        VersionKind::Update => update_facts(&snapshot, input.verdict, input.revision.clone())?,
    };
    let parent = snapshot.parent.as_ref();
    let unchanged = kind == VersionKind::SelectedThreads
        && parent.is_some_and(|parent| {
            parent.sequence == snapshot.guard.sequence && parent.summary.verdict == facts.verdict
        });
    let (result, version, sha256) = if unchanged {
        let hash = parent
            .map(|parent| parent.summary.reference.hash.clone())
            .ok_or(AppError::Internal)?;
        (ResultKind::Unchanged, None, hash)
    } else {
        let version = new_version(&snapshot, facts)
            .map_err(|too_large| AppError::InvalidFields(vec![too_large.field_error()]))?;
        let hash = version.composed.sha256.clone();
        (ResultKind::Final, Some(version), hash)
    };
    let summary = closing_summary(&snapshot, result);
    let run = caller.lead.run_id().clone();
    let refusal = FailureRecord {
        run: run.clone(),
        pass: snapshot.guard.pass,
        kind: FailureKind::ClosingRefused,
        reason: CLOSING_REFUSED.to_owned(),
        sequence: snapshot.guard.sequence,
        turn: caller.turn.clone(),
    };
    let current = snapshot.guard.head.clone();
    Ok(PreparedClosing {
        commit: PublishCommit {
            guard: snapshot.guard,
            kind: CommitKind::Closing(Box::new(ClosingCommit {
                call: caller,
                summary: summary.clone(),
                assessments: input.assessments.clone(),
                version,
            })),
        },
        run,
        sha256,
        current,
        summary,
        refusal,
    })
}

fn closing_error(error: ClosingError) -> FieldError {
    match error {
        ClosingError::FullReviewInSelectedThreads => FieldError::new(
            "/fullReview",
            "This pass checks selected threads, so it cannot close as a full review. Leave out fullReview.",
        ),
        ClosingError::AssessmentsWithoutSelectedThreads => FieldError::new(
            "/assessments",
            "No selected-thread check is open, so leave out assessments.",
        ),
        ClosingError::MissingVerdict => FieldError::new(
            "/verdict",
            "A full review, which includes the first version of a review, needs a verdict.",
        ),
        ClosingError::MissingRevision => FieldError::new(
            "/revision",
            "A full review, which includes the first version of a review, needs the reviewed revision: {\"sha\":\"<full SHA>\"}, or {\"sha\":null,\"reason\":\"…\"} when it is unknown.",
        ),
    }
}

/// One error per active finding of the current version that a full review neither changed
/// nor assessed, naming its ID and title.
fn untouched_errors(snapshot: &LedgerSnapshot) -> Vec<FieldError> {
    let parent_active: Vec<_> = snapshot
        .findings
        .iter()
        .filter(|finding| finding.parent.is_some())
        .map(|finding| finding.record.id)
        .collect();
    let touched: Vec<_> = snapshot
        .findings
        .iter()
        .filter(|finding| finding.touched)
        .map(|finding| finding.record.id)
        .collect();
    untouched_findings(&parent_active, &touched)
        .into_iter()
        .filter_map(|id| {
            snapshot
                .findings
                .iter()
                .find(|finding| finding.record.id == id)
        })
        .map(|finding| {
            FieldError::new(
                "",
                format!(
                    "{} \"{}\" is on the current version but this review neither changed nor assessed it. Record what became of it with assess_finding, or change it with record_finding.",
                    finding.record.id, finding.record.fields.title
                ),
            )
        })
        .collect()
}

/// A selected-thread pass reviews the frozen batch's checked SHA, so a stated revision must
/// be that SHA.
fn revision_errors(snapshot: &LedgerSnapshot, input: &FinishReview) -> Vec<FieldError> {
    let Some(batch) = snapshot.pass.as_ref().and_then(|pass| pass.batch.as_ref()) else {
        return Vec::new();
    };
    match &input.revision {
        Some(RevisionEvidence::Exact(sha)) if *sha == batch.checked_sha => Vec::new(),
        Some(_) => vec![FieldError::new(
            "/revision",
            format!(
                "This check covers {}. Leave out revision, or state that SHA.",
                batch.checked_sha
            ),
        )],
        None => Vec::new(),
    }
}

fn full_review_facts(
    snapshot: &LedgerSnapshot,
    input: &FinishReview,
) -> Result<VersionFacts, AppError> {
    let reviewed = input.revision.clone().ok_or(AppError::Internal)?;
    let exact = exact_sha(&reviewed);
    Ok(VersionFacts {
        kind: VersionKind::FullReview,
        verdict: input.verdict.ok_or(AppError::Internal)?,
        coverage: Coverage::FullMr,
        coverage_source_sha: exact.clone(),
        coverage_target_sha: snapshot.cover.target_sha.clone(),
        full_mr_reviewed_sha: exact,
        reviewed,
        checked_at_ms: snapshot.cover.read_at_ms,
        new_attention: true,
    })
}

fn selected_threads_facts(
    snapshot: &LedgerSnapshot,
    input: &FinishReview,
) -> Result<VersionFacts, AppError> {
    let parent = snapshot.parent.as_ref().ok_or(AppError::Internal)?;
    let batch = snapshot
        .pass
        .as_ref()
        .and_then(|pass| pass.batch.as_ref())
        .ok_or(AppError::Internal)?;
    Ok(VersionFacts {
        kind: VersionKind::SelectedThreads,
        verdict: input.verdict.unwrap_or(parent.summary.verdict),
        reviewed: RevisionEvidence::Exact(batch.checked_sha.clone()),
        coverage: Coverage::SelectedThreads,
        coverage_source_sha: Some(batch.checked_sha.clone()),
        coverage_target_sha: batch.target_sha.clone(),
        full_mr_reviewed_sha: parent.summary.full_mr_reviewed_sha.clone(),
        checked_at_ms: snapshot.cover.read_at_ms,
        new_attention: true,
    })
}

/// An update carries the parent's verdict, reviewed revision, checked time and coverage,
/// except a verdict or revision the closing call states. It takes a new attention identity
/// only when its active findings, their blocking values or its verdict differ from the
/// parent's.
fn update_facts(
    snapshot: &LedgerSnapshot,
    verdict: Option<Verdict>,
    revision: Option<RevisionEvidence>,
) -> Result<VersionFacts, AppError> {
    let parent = snapshot.parent.as_ref().ok_or(AppError::Internal)?;
    let summary = &parent.summary;
    let verdict = verdict.unwrap_or(summary.verdict);
    let (reviewed, checked_at_ms) = match revision {
        Some(revision) => (revision, snapshot.cover.read_at_ms),
        None => (parent_reviewed(summary), parent.checked_at_ms),
    };
    let findings_differ = snapshot.findings.iter().any(|finding| {
        match (finding.parent, finding.record.is_active()) {
            (None, active) => active,
            (Some(_), false) => true,
            (Some(parent), true) => parent.blocking != finding.record.fields.blocking,
        }
    });
    Ok(VersionFacts {
        kind: VersionKind::Update,
        verdict,
        reviewed,
        coverage: summary.coverage,
        coverage_source_sha: summary.coverage_source_sha.clone(),
        coverage_target_sha: summary.coverage_target_sha.clone(),
        full_mr_reviewed_sha: summary.full_mr_reviewed_sha.clone(),
        checked_at_ms,
        new_attention: findings_differ || verdict != summary.verdict,
    })
}

fn parent_reviewed(summary: &ReportSummary) -> RevisionEvidence {
    match (&summary.reviewed_sha, &summary.revision_reason) {
        (Some(sha), _) => RevisionEvidence::Exact(sha.clone()),
        (None, reason) => RevisionEvidence::Unknown(reason.clone().unwrap_or_default()),
    }
}

fn exact_sha(revision: &RevisionEvidence) -> Option<GitSha> {
    match revision {
        RevisionEvidence::Exact(sha) => Some(sha.clone()),
        RevisionEvidence::Unknown(_) => None,
    }
}

/// Composes the version and its changes, or refuses text over `MAX_COMPOSED_BYTES`.
fn new_version(snapshot: &LedgerSnapshot, facts: VersionFacts) -> Result<NewVersion, TooLarge> {
    let composed = compose::compose(&snapshot.cover, &facts, &snapshot.findings, &snapshot.notes);
    if composed.text.len() > MAX_COMPOSED_BYTES {
        return Err(TooLarge::of(snapshot, composed.text.len()));
    }
    let changes = ReportEdits {
        findings: snapshot
            .findings
            .iter()
            .filter_map(|finding| {
                let outcome = recorded_outcome(finding)?;
                Some(FindingEdit {
                    finding_id: finding.record.id.to_string(),
                    conclusion: outcome.conclusion,
                    reason: outcome.reason.clone(),
                    checked_sha: checked_revision(outcome, &facts.reviewed).cloned(),
                    discussion_ids: outcome.discussion_ids.clone(),
                })
            })
            .collect(),
    };
    Ok(NewVersion {
        facts,
        composed,
        changes,
    })
}

/// The acknowledgement's lists: each finding by its state against the parent version, the
/// findings subagents filed or changed, and the reported notes that are missing.
fn closing_summary(snapshot: &LedgerSnapshot, result: ResultKind) -> ClosingSummary {
    let mut summary = ClosingSummary {
        result,
        new: Vec::new(),
        updated: Vec::new(),
        retired: Vec::new(),
        carried: 0,
        by_subagents: Vec::new(),
        empty_sections: REPORTED_NOTES
            .into_iter()
            .filter(|section| {
                !snapshot
                    .notes
                    .iter()
                    .any(|note| note.key.section() == *section)
            })
            .collect(),
    };
    for finding in &snapshot.findings {
        let id = finding.record.id;
        match state(finding) {
            Some(VersionState::New) => summary.new.push(id),
            Some(VersionState::Updated) => summary.updated.push(id),
            Some(VersionState::Retired) => summary.retired.push(id),
            Some(VersionState::Carried) => summary.carried += 1,
            None => {}
        }
        if finding.by_subagent {
            summary.by_subagents.push(id);
        }
    }
    summary
}

/// Composed text over `MAX_COMPOSED_BYTES`, with the largest records to shorten.
struct TooLarge {
    bytes: usize,
    /// The largest records' names and sizes in bytes, largest first.
    largest: Vec<(String, usize)>,
}

impl TooLarge {
    fn of(snapshot: &LedgerSnapshot, bytes: usize) -> Self {
        let findings = snapshot
            .findings
            .iter()
            .filter(|finding| finding.record.is_active())
            .map(|finding| {
                (
                    finding.record.id.to_string(),
                    finding_bytes(&finding.record.fields),
                )
            });
        let notes = snapshot.notes.iter().map(|note| {
            let name = match note.key.title() {
                Some(title) => format!("the extra note \"{title}\""),
                None => format!("the {} note", note.key.section()),
            };
            (name, note.body.len())
        });
        let mut largest: Vec<_> = findings.chain(notes).collect();
        largest.sort_by_key(|(_, bytes)| Reverse(*bytes));
        largest.truncate(NAMED_RECORDS);
        Self { bytes, largest }
    }

    fn field_error(&self) -> FieldError {
        FieldError::new("", self.to_string())
    }
}

impl Display for TooLarge {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        write!(
            output,
            "The report would be {} KiB, over the {} MiB limit. Shorten the largest records:",
            self.bytes.div_ceil(1024),
            MAX_COMPOSED_BYTES / (1024 * 1024)
        )?;
        for (index, (name, bytes)) in self.largest.iter().enumerate() {
            let separator = if index == 0 { " " } else { ", " };
            write!(output, "{separator}{name} ({} KiB)", bytes.div_ceil(1024))?;
        }
        output.write_str(".")
    }
}
