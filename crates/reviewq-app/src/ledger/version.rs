//! What a version is made from and what its commit carries: the closing call's input and
//! acknowledgement, the snapshot publishing reads, the composed text and the conditional
//! commit with its outcome.

use std::ops::Range;

use reviewq_domain::identity::{
    GitSha, MergeRequestIid, NativeTurnId, ReceiptId, ReportHash, Revision, RunId,
};
use reviewq_domain::ledger::{FindingId, RecordRevision, VersionKind};
use reviewq_domain::session::LeadTarget;

use super::{Caller, FindingRecord, NoteRecord, NoteSection, OpenPass, PassId};
use crate::access::FieldError;
use crate::followups::ResultKind;
use crate::reports::{
    ArtifactStats, Assessment, Coverage, FindingDisposition, ReportAuthority, ReportEdits,
    ReportSummary, RevisionEvidence, Verdict,
};

/// `finish_review`'s input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinishReview {
    pub verdict: Option<Verdict>,
    pub revision: Option<RevisionEvidence>,
    pub full_review: bool,
    /// Only in a selected-thread pass: one per selected discussion.
    pub assessments: Vec<Assessment>,
}

/// The acknowledgement of a closing call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinishReceipt {
    /// The version published, or the current one for an `unchanged` batch result.
    pub receipt: ReceiptId,
    /// That version's SHA-256.
    pub sha256: ReportHash,
    pub summary: ClosingSummary,
}

/// Everything in a closing acknowledgement except the version's identity, which the commit
/// assigns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosingSummary {
    /// `Final` when a version was published, `Unchanged` when the batch result added none.
    pub result: ResultKind,
    pub new: Vec<FindingId>,
    pub updated: Vec<FindingId>,
    pub retired: Vec<FindingId>,
    pub carried: u32,
    /// Findings a subagent filed or changed since the parent version.
    pub by_subagents: Vec<FindingId>,
    /// Which of the `summary`, `rationale`, `flow`, `changes` and `verification` notes are
    /// missing.
    pub empty_sections: Vec<NoteSection>,
}

/// What `finish_review` reads before composing.
pub enum ClosingRead {
    /// The same call was already answered. Return its acknowledgement unchanged.
    Replayed(FinishReceipt),
    Current(Box<ClosingSnapshot>),
    /// The lead may not close the run now. Record `closing_refused`, then refuse the call
    /// with `Conflict`.
    Refused(RefusedClosing),
}

/// A closing call from a run's lead that the store refuses, with what its `closing_refused`
/// failure records. It names the run rather than a `Caller`, because the calling session may
/// no longer be the run's registered lead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefusedClosing {
    pub run: RunId,
    /// The call's native turn, when the host named it.
    pub turn: Option<NativeTurnId>,
    /// The run's pass that is not closed, which the failure names.
    pub pass: Option<PassId>,
    /// The run's ledger sequence, which the failure is recorded at.
    pub sequence: Revision,
}

/// A new closing call with the snapshot it composes from.
pub struct ClosingSnapshot {
    /// The run's lead, as the store attributed the call.
    pub caller: Caller,
    /// The lead's last closing call on the run that committed.
    pub last_closing: Option<LastClosing>,
    pub snapshot: LedgerSnapshot,
}

/// A committed closing call, which a repeat with nothing changed answers again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LastClosing {
    pub ack: FinishReceipt,
    /// The run's ledger sequence it committed at. An unchanged sequence means no record
    /// changed since.
    pub sequence: Revision,
    /// The assessments it closed a selected-thread pass with, so a repeat of them is told
    /// apart from assessments without a pass.
    pub assessments: Vec<Assessment>,
}

/// Everything composing and the closing checks read, from one read transaction.
pub struct LedgerSnapshot {
    pub guard: SnapshotGuard,
    pub cover: CoverFacts,
    /// The run's current version, which a new version follows.
    pub parent: Option<ParentVersion>,
    pub pass: Option<OpenPass>,
    /// In ID order: every active finding, every finding on the parent version and every
    /// finding changed since it.
    pub findings: Vec<SnapshotFinding>,
    /// Fixed sections in layout order, then extra notes in the order first filed.
    pub notes: Vec<NoteRecord>,
}

/// The state a commit rechecks. Copy it from the snapshot into the commit unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotGuard {
    pub lead: LeadTarget,
    /// The workspace, grant and settings authority the version binds.
    pub authority: ReportAuthority,
    /// The run's ledger sequence.
    pub sequence: Revision,
    /// The run's disposition revision: the version's starting one.
    pub disposition_revision: Revision,
    /// The current version, the parent of the one committed.
    pub head: Option<ReceiptId>,
    pub pass: Option<PassId>,
    /// The frozen batch of a selected-thread pass.
    pub batch: Option<String>,
}

/// ReviewQ's own facts for the first line and the cover lines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverFacts {
    pub project_path: String,
    pub iid: MergeRequestIid,
    pub mr_url: String,
    /// The report's title when no `title` note exists.
    pub mr_title: String,
    /// The MR's target-branch SHA, when ReviewQ knows it.
    pub target_sha: Option<GitSha>,
    /// When the snapshot was read, in Unix milliseconds: the `Checked at` time of a version
    /// that does not carry its parent's (`VersionFacts::checked_at_ms`).
    pub read_at_ms: i64,
}

/// The run's current version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParentVersion {
    pub summary: ReportSummary,
    pub kind: VersionKind,
    /// The ledger sequence it published.
    pub sequence: Revision,
    /// Its `Checked at` time, in Unix milliseconds. An update carries it unless its closing
    /// call states a new revision.
    pub checked_at_ms: i64,
}

/// A finding with what composing needs beyond its record.
#[derive(Clone)]
pub struct SnapshotFinding {
    pub record: FindingRecord,
    /// Its row among the parent version's active findings.
    pub parent: Option<ParentFinding>,
    /// The current outcome was recorded after the parent version was published.
    pub outcome_is_new: bool,
    /// Changed or assessed after the open pass opened, or after the parent version when no
    /// pass is open.
    pub touched: bool,
    /// A subagent filed or changed it after the parent version.
    pub by_subagent: bool,
    /// The user's disposition at `guard.disposition_revision`. Its blocking value, when set,
    /// overrides the lead's.
    pub disposition: Option<FindingDisposition>,
    /// Monitored discussions linked to the finding at that revision.
    pub discussion_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParentFinding {
    pub revision: RecordRevision,
    /// The lead's blocking value on the parent version.
    pub blocking: bool,
}

/// One publish, committed conditionally on its guard.
#[derive(Debug)]
pub struct PublishCommit {
    /// The snapshot's guard, unchanged.
    pub guard: SnapshotGuard,
    pub kind: CommitKind,
}

#[derive(Debug)]
pub enum CommitKind {
    /// `finish_review`.
    Closing(Box<ClosingCommit>),
    /// An update owed with no pass open. It completes no batch.
    Update(Box<NewVersion>),
}

/// What a closing call commits.
#[derive(Debug)]
pub struct ClosingCommit {
    pub call: Caller,
    pub summary: ClosingSummary,
    /// Only in a selected-thread pass.
    pub assessments: Vec<Assessment>,
    /// `None` only for a selected-thread pass in which no record changed and the verdict
    /// equals the current version's. The app decides that, and the batch then completes
    /// `unchanged` against the current version.
    pub version: Option<NewVersion>,
}

/// A composed version and its rows.
#[derive(Debug)]
pub struct NewVersion {
    pub facts: VersionFacts,
    pub composed: Composed,
    /// One entry per outcome recorded on this version, with its checked revision resolved.
    pub changes: ReportEdits,
}

/// The version's own fields, decided by the app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionFacts {
    pub kind: VersionKind,
    pub verdict: Verdict,
    pub reviewed: RevisionEvidence,
    pub coverage: Coverage,
    pub coverage_source_sha: Option<GitSha>,
    pub coverage_target_sha: Option<GitSha>,
    /// Advanced only by a full review; otherwise the parent's.
    pub full_mr_reviewed_sha: Option<GitSha>,
    /// The cover's `Checked at` time, in Unix milliseconds, stored with the version: the
    /// parent's for an update whose closing call states no new revision, otherwise the
    /// snapshot's `read_at_ms`.
    pub checked_at_ms: i64,
    /// The version takes its own attention identity and raises its notification. An update
    /// whose active findings, blocking values and verdict equal its parent's keeps the
    /// parent's.
    pub new_attention: bool,
}

/// The composed report text and where each card and section sits in it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Composed {
    pub text: String,
    pub sha256: ReportHash,
    pub stats: ArtifactStats,
    /// One per active finding, in text order. The store copies each row's other columns from
    /// the record at the guarded ledger sequence, writing `default_category` for a record
    /// without a category.
    pub cards: Vec<CardSpan>,
    pub sections: Vec<SectionSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CardSpan {
    pub id: FindingId,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionSpan {
    /// The heading text without its `#` marks.
    pub title: String,
    pub span: Span,
}

/// A slice of the composed text, by bytes and by Unicode scalar values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Span {
    pub bytes: Range<usize>,
    pub scalar_offset: u64,
    pub scalar_length: u64,
}

/// How a publish commit ended, by the rules `LedgerStore::commit_publish` states; a refused
/// commit is `Err(AppError::Conflict)` instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishOutcome {
    /// The version was stored under this receipt.
    Published(ReceiptId),
    /// The batch completed `unchanged`; no version was added.
    Unchanged,
    /// A record or disposition changed after the snapshot, another version took the head, or
    /// a pass opened before an update committed. Nothing was written; read a new snapshot and
    /// decide again.
    Stale,
    /// The assessments do not match the frozen batch. Nothing was written.
    Invalid(Vec<FieldError>),
}
