//! The findings ledger: what the six ledger tools file and read, what publishing reads and
//! commits, and the store port that runs each of them as one short transaction or read.
//!
//! The protocol checks field shapes, sizes, one-line fields and titles. This module owns the Markdown rules
//! (`markdown`) and the per-run limits. The store owns caller binding, revisions, duplicates,
//! replay and the counts behind the per-run limits, because only its transaction sees them.
//! Composing happens outside every transaction.

pub mod compose;
pub mod markdown;
pub mod passes;
pub mod publish;
pub mod readback;
pub mod record;
mod version;

pub use version::{
    CardSpan, ClosingCommit, ClosingRead, ClosingSnapshot, ClosingSummary, CommitKind, Composed,
    CoverFacts, FinishReceipt, FinishReview, LastClosing, LedgerSnapshot, NewVersion,
    ParentFinding, ParentVersion, PublishCommit, PublishOutcome, SectionSpan, SnapshotFinding,
    SnapshotGuard, Span, VersionFacts,
};

use reviewq_domain::identity::{
    GitSha, InvalidIdentity, NativeSessionId, NativeTurnId, ReceiptId, Revision, RunId, SessionKey,
};
use reviewq_domain::ledger::{
    CallerRole, FailureKind, FindingId, GateDecision, PassKind, PassState, RecordRevision,
    SettleFacts, Tool,
};
use reviewq_domain::session::{HostProvider, LeadTarget};

use crate::access::AppFuture;
use crate::labels::labels;
use crate::reports::{
    FindingBasis, FindingConclusion, FindingDetails, FindingLocation, FindingSection, Severity,
};

/// Active findings one run may hold. The store refuses a write that would exceed it.
pub const MAX_ACTIVE_FINDINGS: u32 = 200;
/// `extra` notes one run may hold. The store refuses a write that would exceed it.
pub const MAX_EXTRA_NOTES: u32 = 12;
/// Budget of one `review_ledger` page: the sum of the UTF-8 bytes of the text fields of the
/// records on it. A page always holds at least one record, which the field limits keep within
/// the budget.
pub const MAX_LEDGER_PAGE_BYTES: usize = 64 * 1024;

labels!(NoteSection {
    Title => "title",
    Summary => "summary",
    Rationale => "rationale",
    Flow => "flow",
    Changes => "changes",
    Threads => "threads",
    Questions => "questions",
    Verification => "verification",
    Publication => "publication",
    Extra => "extra",
});

labels!(RecordField {
    Title => "title",
    Severity => "severity",
    Blocking => "blocking",
    Section => "section",
    Category => "category",
    Locations => "locations",
    Issue => "issue",
    Consequence => "consequence",
    Evidence => "evidence",
    ProposedComment => "proposedComment",
    Details => "details",
    Outcome => "outcome",
    Reason => "reason",
    DiscussionIds => "discussionIds",
    CheckedSha => "checkedSha",
    Body => "body",
});

/// The category a finding shows when it names none: its section's name.
pub fn default_category(section: FindingSection) -> &'static str {
    match section {
        FindingSection::Functional => "Functional",
        FindingSection::Quality => "Quality",
    }
}

/// The calling session as the call itself names it. The store attributes it to a run's lead
/// or a verified child, or refuses it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostSession {
    /// Claude's plugin: the ReviewQ session key of the linked lead conversation, and the
    /// subagent's ID when a subagent calls.
    Claude {
        session: SessionKey,
        agent: Option<String>,
    },
    /// Codex's MCP request: the calling thread, and its parent thread from the turn metadata
    /// when present. A child is verified from that parent, when it is the lead or an already
    /// bound child of the run, or else from the children ReviewQ discovered.
    Codex {
        thread: NativeSessionId,
        parent: Option<NativeSessionId>,
    },
}

/// One ledger call's identity. A repeat with the same session and call ID returns the stored
/// result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallBinding {
    pub session: HostSession,
    /// Claude's tool-use ID, or the Codex MCP call ID.
    pub call_id: String,
    /// The native turn the call belongs to, when the host names it.
    pub turn: Option<NativeTurnId>,
}

/// A call the store attributed to a run's lead or to one of its verified children.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Caller {
    /// The run's registered lead.
    pub lead: LeadTarget,
    /// The calling native session: the lead's own for Claude, the calling thread for Codex.
    pub session: NativeSessionId,
    /// The Claude subagent's ID when a subagent calls.
    pub agent: Option<String>,
    pub call_id: String,
    pub turn: Option<NativeTurnId>,
}

impl Caller {
    /// The lead only when its own session calls without a subagent ID.
    pub fn role(&self) -> CallerRole {
        if self.agent.is_none() && &self.session == self.lead.binding().identity().native_id() {
            CallerRole::Lead
        } else {
            CallerRole::Child
        }
    }

    pub fn provider(&self) -> HostProvider {
        self.lead.binding().identity().provider()
    }
}

/// Who last changed a record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Author {
    Lead,
    /// A child, by the Claude subagent's ID or the Codex child's thread ID.
    Subagent(String),
}

impl Author {
    /// The author of a change `caller` made.
    pub fn of(caller: &Caller) -> Self {
        match caller.role() {
            CallerRole::Lead => Self::Lead,
            CallerRole::Child => Self::Subagent(
                caller
                    .agent
                    .clone()
                    .unwrap_or_else(|| caller.session.as_str().to_owned()),
            ),
        }
    }
}

/// The store's identity of one pass.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PassId(i64);

impl PassId {
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for PassId {
    type Error = InvalidIdentity;

    /// Fails for zero and negative values.
    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value < 1 {
            return Err(InvalidIdentity);
        }
        Ok(Self(value))
    }
}

/// Every field of a finding at one revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingFields {
    pub title: String,
    pub severity: Severity,
    pub blocking: bool,
    pub section: FindingSection,
    /// `None` shows [`default_category`].
    pub category: Option<String>,
    /// Locations, issue, consequence, evidence, proposed comment and details.
    pub body: FindingDetails,
}

/// The fields a change gives. A field left `None` keeps its value.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FindingPatch {
    pub title: Option<String>,
    pub severity: Option<Severity>,
    pub blocking: Option<bool>,
    pub section: Option<FindingSection>,
    pub category: Option<String>,
    pub locations: Option<Vec<FindingLocation>>,
    pub issue: Option<String>,
    pub consequence: Option<String>,
    pub evidence: Option<FindingBasis>,
    pub proposed_comment: Option<String>,
    pub details: Option<String>,
}

/// What became of a finding, as `assess_finding` recorded it. A retiring conclusion
/// (`FindingConclusion::retires_finding`) removes the finding from the active set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingOutcome {
    pub conclusion: FindingConclusion,
    /// One line of 1–1,024 characters.
    pub reason: String,
    pub discussion_ids: Vec<String>,
    /// The revision the agent checked, when it gave one. Publishing falls back to the closing
    /// call's reviewed SHA, then to unknown.
    pub checked_sha: Option<GitSha>,
}

/// A finding at its current revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingRecord {
    pub id: FindingId,
    pub revision: RecordRevision,
    pub author: Author,
    pub fields: FindingFields,
    /// The latest outcome. Changing a retired finding reopens it and clears its outcome.
    pub outcome: Option<FindingOutcome>,
}

impl FindingRecord {
    /// Whether the finding is in the active set: no outcome, or one that keeps it.
    pub fn is_active(&self) -> bool {
        self.outcome
            .as_ref()
            .is_none_or(|outcome| !outcome.conclusion.retires_finding())
    }
}

/// Which note a write or read names: a fixed section, or an `extra` note by its title.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteKey {
    section: NoteSection,
    title: Option<String>,
}

impl NoteKey {
    /// The note of a fixed section. `None` for `Extra`, which needs a title.
    pub fn fixed(section: NoteSection) -> Option<Self> {
        (section != NoteSection::Extra).then_some(Self {
            section,
            title: None,
        })
    }

    /// The `extra` note with this title.
    pub fn extra(title: String) -> Self {
        Self {
            section: NoteSection::Extra,
            title: Some(title),
        }
    }

    pub fn section(&self) -> NoteSection {
        self.section
    }

    /// The title of an `extra` note; `None` for a fixed section.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}

/// A note at its current revision. The body of the `title` note is the report's title.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteRecord {
    pub key: NoteKey,
    pub revision: RecordRevision,
    pub body: String,
}

/// One record write: `record_finding`, `assess_finding` or `record_note`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordWrite {
    /// A new finding. The store assigns the next unused ID, or returns an equal active
    /// finding under the duplicate rules.
    CreateFinding(Box<FindingFields>),
    /// Changes the fields `patch` gives. Changing a retired finding reopens it.
    ChangeFinding {
        id: FindingId,
        expected_revision: RecordRevision,
        patch: Box<FindingPatch>,
    },
    AssessFinding(FindingAssessment),
    WriteNote(NoteWrite),
}

impl RecordWrite {
    /// The tool that makes this write.
    pub fn tool(&self) -> Tool {
        match self {
            Self::CreateFinding(_) | Self::ChangeFinding { .. } => Tool::RecordFinding,
            Self::AssessFinding(_) => Tool::AssessFinding,
            Self::WriteNote(_) => Tool::RecordNote,
        }
    }
}

/// `assess_finding`: records what became of a finding already on the run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingAssessment {
    pub id: FindingId,
    pub expected_revision: RecordRevision,
    pub outcome: FindingOutcome,
}

/// `record_note`: replaces one note. An empty body removes it. A removed note keeps its
/// revision count, so filing it again continues from there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteWrite {
    pub key: NoteKey,
    pub body: String,
    /// The revision the caller last read. Required while the note exists.
    pub expected_revision: Option<RecordRevision>,
}

/// The result of an accepted, equal or replayed record write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordReceipt {
    Finding {
        id: FindingId,
        revision: RecordRevision,
        /// Records of the run with a change no published version shows yet.
        unpublished: u32,
    },
    Note {
        key: NoteKey,
        revision: RecordRevision,
        /// Records of the run with a change no published version shows yet.
        unpublished: u32,
    },
}

/// An accepted, equal or replayed record write, with the run it landed on so the caller can
/// settle that run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recorded {
    pub run: RunId,
    pub receipt: RecordReceipt,
}

/// A record write the store refused. Nothing was written. The app words each as a field
/// error the agent corrects in its turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriteRefusal {
    /// `id` names no finding of this run.
    UnknownFinding,
    /// The expected revision is not current. `changed` names the fields that differ between
    /// the expected and the current revision.
    StaleRevision {
        current: RecordRevision,
        changed: Vec<RecordField>,
    },
    /// The note exists, so the write must give its expected revision.
    RevisionRequired { current: RecordRevision },
    /// An empty body names a note that does not exist.
    UnknownNote,
    /// A new finding matches active finding `id`: equal titles and equal first locations, or
    /// equal titles and no locations on either. Its other content differs, so the caller
    /// changes `id` instead.
    DuplicateFinding { id: FindingId },
    /// The run would hold more than `MAX_ACTIVE_FINDINGS` active findings.
    TooManyFindings,
    /// The run would hold more than `MAX_EXTRA_NOTES` extra notes.
    TooManyExtraNotes,
}

/// `review_ledger`: one page of the run's ledger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerRead {
    pub selection: LedgerSelection,
    /// The `next` cursor of the previous page.
    pub after: Option<LedgerCursor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LedgerSelection {
    /// One line per active finding in ID order, then one per note.
    Short,
    /// These findings in full, in ID order. The protocol allows at most 10.
    Findings(Vec<FindingId>),
    /// These sections' notes in full. `Extra` returns every extra note in the order first
    /// filed.
    Notes(Vec<NoteSection>),
}

/// Where the next page starts: the index of its first entry in the selection's order. Valid
/// while the run's ledger sequence stays at `sequence`; a later read returns `CursorExpired`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LedgerCursor {
    pub sequence: Revision,
    pub position: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerPage {
    /// The run's current version.
    pub receipt: Option<ReceiptId>,
    pub pass: Option<OpenPass>,
    pub entries: LedgerEntries,
    /// Requested IDs that name no finding of this run.
    pub unknown: Vec<FindingId>,
    pub next: Option<LedgerCursor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LedgerEntries {
    Short {
        findings: Vec<FindingLine>,
        notes: Vec<NoteLine>,
    },
    Full {
        findings: Vec<FindingRecord>,
        notes: Vec<NoteRecord>,
    },
}

/// One active finding in a short list or the read-back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingLine {
    pub id: FindingId,
    pub revision: RecordRevision,
    pub severity: Severity,
    /// The lead's blocking value.
    pub blocking: bool,
    pub title: String,
    pub author: Author,
}

/// One note in a short list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteLine {
    pub key: NoteKey,
    pub revision: RecordRevision,
    /// The body's length in Unicode scalar values.
    pub characters: u32,
}

/// The run's pass that is not closed: what it owes and for which discussions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenPass {
    pub id: PassId,
    pub kind: PassKind,
    /// `Open` or `Waiting`.
    pub state: PassState,
    /// The frozen batch of a selected-thread pass.
    pub batch: Option<FrozenBatch>,
}

/// The selected-thread batch a pass closes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenBatch {
    pub id: String,
    /// The source SHA the batch froze. It is the reviewed revision of the version that closes
    /// the pass.
    pub checked_sha: GitSha,
    pub target_sha: Option<GitSha>,
    /// The selected discussions, each owed one assessment.
    pub discussion_ids: Vec<String>,
}

/// `needs_input`: the pass it held.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoldReceipt {
    pub pass: PassKind,
    /// Records of the run with a change no published version shows yet.
    pub unpublished: u32,
}

/// A host's stop hook asking whether the turn may end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GateRequest {
    pub session: HostSession,
    pub turn: Option<NativeTurnId>,
    /// The host's `stopHookActive`: this stop follows a block.
    pub stop_hook_active: bool,
}

/// The gate's decision and what a block reason names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GateAnswer {
    pub decision: GateDecision,
    pub pass: Option<OpenPass>,
    pub active_findings: u32,
}

/// What the read-back after compaction shows the lead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadBack {
    pub pass: Option<OpenPass>,
    /// Every active finding, newest change first.
    pub findings: Vec<FindingLine>,
}

/// A run that may need settling, with the facts `settle` decides from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettleCandidate {
    pub run: RunId,
    /// The run's ledger sequence when read.
    pub sequence: Revision,
    /// The pass `facts.pass` describes.
    pub pass: Option<PassId>,
    /// Findings on file, for an `unfinished_review` reason.
    pub active_findings: u32,
    pub facts: SettleFacts,
}

/// A delivery failure to record once per run, pass and kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureRecord {
    pub run: RunId,
    pub pass: Option<PassId>,
    pub kind: FailureKind,
    /// Shown with the failed delivery: 1–2,048 bytes.
    pub reason: String,
    /// The ledger sequence the failure was seen at.
    pub sequence: Revision,
    /// The refused closing call, for `closing_refused`.
    pub call: Option<Caller>,
}

/// The ledger's storage. Each write is one short transaction and each read one read
/// transaction; none reads a transcript or asks a provider. A call that cannot be attributed
/// to the run's lead or a verified child fails with `Forbidden`.
pub trait LedgerStore: Send + Sync {
    /// Applies one record write. A known call returns its stored result, or
    /// `IdempotencyConflict` when that call made another write. Checks the duplicate rules,
    /// the expected revision and the per-run limits, and returns a broken rule as a
    /// `WriteRefusal` with nothing written. Otherwise writes the record, appends its full
    /// state to the change history, advances the run's ledger sequence, opens a full-review
    /// pass when `opens_pass` says so and stores the result for replay. Never reads or
    /// changes a user's disposition.
    fn write_record(
        &self,
        binding: CallBinding,
        write: RecordWrite,
    ) -> AppFuture<'_, Result<Recorded, WriteRefusal>>;

    /// Reads one page of the run's ledger, holding whole records up to
    /// `MAX_LEDGER_PAGE_BYTES`. `CursorExpired` when the ledger sequence moved since `after`
    /// was issued.
    fn read_ledger(&self, session: HostSession, read: LedgerRead) -> AppFuture<'_, LedgerPage>;

    /// Holds the run's open pass for `needs_input` until the lead's next turn starts, and
    /// returns `None` with no pass open. `Forbidden` for a child. A known call returns its
    /// stored result. Completes no assessment.
    fn hold_pass(&self, binding: CallBinding, reason: String)
    -> AppFuture<'_, Option<HoldReceipt>>;

    /// Answers one stop request: applies `gate` to the stored facts, stores the decision's
    /// block count for the turn on the pass, and notes that the provider's stop hook
    /// reported, rewriting that time only when it is more than an hour old. The report is
    /// noted even for a session in no run, because the Codex hook runs for every session; such
    /// a session is released.
    fn answer_gate(&self, request: GateRequest) -> AppFuture<'_, GateAnswer>;

    /// Reads what `finish_review` composes from. `Forbidden` unless the caller is the run's
    /// lead itself. A known call returns `Replayed`. `Conflict` when the run has no current
    /// workspace or grant authority.
    fn closing_snapshot(&self, binding: CallBinding) -> AppFuture<'_, ClosingRead>;

    /// Reads what an update composes from. `NotFound` for an unknown run, `Conflict` when it
    /// has no current workspace or grant authority.
    fn update_snapshot(&self, run: RunId) -> AppFuture<'_, LedgerSnapshot>;

    /// Commits a publish after rechecking its guard, in this order. A changed ledger
    /// sequence, disposition revision or head is `Stale`, even when the pass or batch changed
    /// too, so a closing call that lost a race recomposes instead of being refused. Then a
    /// changed lead, authority, pass or frozen batch is `Conflict`. Then assessments that do
    /// not cover the frozen batch are `Invalid`. Each writes nothing. Otherwise, in one transaction: seals the text in `CHUNK_BYTES` chunks,
    /// appends the version with its finding, section and outcome rows, moves the head, binds
    /// the disposition revisions, closes the pass, completes the batch, resolves earlier
    /// failures, stores the closing acknowledgement for replay, inserts the notification and
    /// emits the change events.
    fn commit_publish(&self, commit: PublishCommit) -> AppFuture<'_, PublishOutcome>;

    /// Builds the reader's display index for a version that just committed. A failure
    /// leaves the version readable, so callers may ignore it.
    fn index_version(&self, run: RunId, receipt: ReceiptId) -> AppFuture<'_, ()>;

    /// The runs with an open pass or unpublished records, with their settle facts from
    /// stored session state, in one indexed read. `Some(run)` reads only that run. Leaves out
    /// a run whose unresolved failure was recorded at its current ledger sequence, so a run
    /// found unfinished, or whose update failed, is not reworked until something changes.
    fn runs_to_settle(&self, run: Option<RunId>) -> AppFuture<'_, Vec<SettleCandidate>>;

    /// Records a failure with one notification when first seen for its run, pass and kind.
    /// A repeat updates the reason and sequence in place and raises nothing. A failure for a
    /// pass that is already closed is stored resolved.
    fn record_failure(&self, failure: FailureRecord) -> AppFuture<'_, ()>;

    /// What the read-back shows, or `None` unless `session` is a run's lead itself.
    fn read_back(&self, session: HostSession) -> AppFuture<'_, Option<ReadBack>>;

    /// When the provider's stop hook last reported on this installation, in Unix
    /// milliseconds, or `None` if it never has.
    fn last_stop_hook_report(&self, provider: HostProvider) -> AppFuture<'_, Option<i64>>;
}
