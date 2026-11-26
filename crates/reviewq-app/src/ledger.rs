//! The findings ledger: what the six ledger tools file and read, what publishing reads and
//! commits, and the store port that runs each of them as one short transaction or read.
//!
//! The protocol checks field shapes, sizes, one-line fields and titles. This module owns the Markdown rules
//! (`markdown`) and the per-run limits. The store owns caller binding, revisions, duplicates,
//! replay and the counts behind the per-run limits, because only its transaction sees them.
//! Composing happens outside every transaction.

mod compose;
mod layout;
mod links;
pub(crate) mod markdown;
pub mod passes;
pub mod publish;
pub mod readback;
pub mod record;
mod refusal;
mod version;

pub use links::{MAX_TICKET_LINKS, ReportLinks};
pub use version::{
    CardSpan, ClosingCommit, ClosingRead, ClosingSnapshot, ClosingSummary, CommitKind, Composed,
    CoverFacts, FinishReceipt, FinishReview, LastClosing, LedgerSnapshot, NewVersion,
    ParentFinding, ParentVersion, PublishCommit, PublishOutcome, RefusedClosing, SectionSpan,
    SnapshotFinding, SnapshotGuard, Span, VersionFacts,
};

use reviewq_domain::identity::{
    GitSha, InvalidIdentity, NativeSessionId, NativeTurnId, ReceiptId, Revision, RunId, SessionKey,
};
use reviewq_domain::ledger::{
    CallerRole, FailureKind, FindingId, GateDecision, PassKind, PassState, RecordRevision,
    SettleFacts, Tool,
};
use reviewq_domain::session::{HostProvider, LeadTarget};

use crate::access::{Actor, AppFuture};
use crate::labels::labels;
use crate::reports::{
    FindingBasis, FindingConclusion, FindingDetails, FindingLocation, FindingSection, Severity,
};

/// Active findings one run may hold. The store refuses a write that would exceed it.
pub const MAX_ACTIVE_FINDINGS: u32 = 200;
/// `extra` notes one run may hold. The store refuses a write that would exceed it.
pub const MAX_EXTRA_NOTES: u32 = 12;
/// Budget of one `review_ledger` page: the sum of the UTF-8 bytes of the text fields of the
/// records on it. A page holds whole records within the budget, and always at least one: a
/// record whose fields are all at their limits can exceed the budget alone, and then fills a
/// page by itself.
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

/// The calling session as a tool call names it. The store attributes the call to the run's
/// lead or to a verified child of it, or refuses it.
///
/// | Caller | Claude | Codex |
/// |---|---|---|
/// | Lead | the lead's session, no agent ID | the lead's own thread |
/// | Child | the lead's session and the subagent's ID | its own thread, whose parent is the lead |
/// | Grandchild | the lead's session and its own subagent ID | its own thread, whose parent is a child |
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostSession {
    /// Claude's plugin: the ReviewQ session key of the linked lead conversation, which every
    /// subagent of it calls through too, and the subagent's ID when a subagent calls. A call
    /// without an agent ID is the lead's own.
    Claude {
        session: SessionKey,
        agent: Option<String>,
    },
    /// Codex's MCP request: the calling thread, its parent thread from the turn metadata when
    /// present, and the request's actor. The store resolves the run's lead only inside a
    /// checkout the actor's active grant covers (`checkout_lead`). The calling thread is the
    /// lead when it is the lead's registered thread. Any other thread is a child, verified as
    /// descending from the lead: through `parent` when that is the lead or an already bound
    /// child of the run, otherwise through the children ReviewQ discovered.
    Codex {
        thread: NativeSessionId,
        parent: Option<NativeSessionId>,
        actor: Actor,
    },
}

/// The session a host hook names: the stop gate and the read-back after compaction. A hook
/// carries no grant and always speaks for the session's own agent, never for a subagent, so
/// only a run's lead session gets a block or a read-back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HookSession {
    /// Claude's hook: the ReviewQ session key of the linked conversation.
    Claude(SessionKey),
    /// Codex's hook: the thread it ran for.
    Codex(NativeSessionId),
}

/// One ledger call's identity. A repeat with the same session and call ID returns the stored
/// result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallBinding {
    /// Who calls, as the call names it. The store decides from it whether the lead or a
    /// child calls.
    pub session: HostSession,
    /// Claude's tool-use ID, or the Codex MCP call ID.
    pub call_id: String,
    /// The native turn the call belongs to, when the host names it.
    pub turn: Option<NativeTurnId>,
}

/// Which agent of a run made a call, as the store attributed it from the `HostSession`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallingAgent {
    /// The run's registered lead itself: a Claude call without a subagent ID, or a Codex call
    /// from the lead's registered thread.
    Lead,
    /// A Claude subagent at any depth, by the ID the plugin supplied. It calls through the
    /// lead's session.
    ClaudeSubagent(String),
    /// A Codex child or grandchild, by its own thread, which descends from the lead's thread.
    CodexThread(NativeSessionId),
}

/// A call the store attributed to a run's lead or to one of its verified children.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Caller {
    /// The run's registered lead.
    pub lead: LeadTarget,
    pub agent: CallingAgent,
    pub call_id: String,
    pub turn: Option<NativeTurnId>,
}

impl CallingAgent {
    /// `Lead` only for the lead itself; every subagent and descendant thread is a child.
    pub fn role(&self) -> CallerRole {
        match self {
            Self::Lead => CallerRole::Lead,
            Self::ClaudeSubagent(_) | Self::CodexThread(_) => CallerRole::Child,
        }
    }
}

impl Caller {
    pub fn role(&self) -> CallerRole {
        self.agent.role()
    }

    pub fn provider(&self) -> HostProvider {
        self.lead.binding().identity().provider()
    }

    /// The native session the call came from: the lead's own for the lead and for a Claude
    /// subagent, and the child's thread for a Codex child.
    pub fn native_session(&self) -> &NativeSessionId {
        match &self.agent {
            CallingAgent::Lead | CallingAgent::ClaudeSubagent(_) => {
                self.lead.binding().identity().native_id()
            }
            CallingAgent::CodexThread(thread) => thread,
        }
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
        match &caller.agent {
            CallingAgent::Lead => Self::Lead,
            CallingAgent::ClaudeSubagent(agent) => Self::Subagent(agent.clone()),
            CallingAgent::CodexThread(thread) => Self::Subagent(thread.as_str().to_owned()),
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

/// The fields a change gives. Each field is `None` when the input left it out, which keeps
/// the stored value, and `Some(value)` when the input gave it, which stores `value` in the
/// field of the same name in `FindingFields`. So applying a patch is one assignment per given
/// field.
///
/// The optional fields can be removed: a present empty list clears the locations
/// (`Some(vec![])`), and a present empty string removes `proposedComment` or `details`
/// (`Some(None)`). `Some(Some(text))` never holds empty text.
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
    pub proposed_comment: Option<Option<String>>,
    pub details: Option<Option<String>>,
}

impl FindingFields {
    /// The UTF-8 bytes of the authored text: title, category, location paths, issue,
    /// consequence, evidence, proposed comment and details.
    pub fn text_bytes(&self) -> usize {
        let body = &self.body;
        self.title.len()
            + self.category.as_ref().map_or(0, String::len)
            + body
                .locations
                .iter()
                .map(|location| location.path.len())
                .sum::<usize>()
            + body.issue.len()
            + body.consequence.len()
            + body.evidence.text.len()
            + body.proposed_comment.as_ref().map_or(0, String::len)
            + body.details.as_ref().map_or(0, String::len)
    }
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
    /// The `next` cursor of the previous page, sent with the same selection.
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

/// Where the next page starts, bound to the run and the selection it was issued for: the
/// index of its first entry in the selection's order. It continues only a read of the same run
/// and selection, while the run's ledger sequence stays at `sequence`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerCursor {
    pub run: RunId,
    pub selection: LedgerSelection,
    pub sequence: Revision,
    pub position: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerPage {
    /// The run's current version.
    pub receipt: Option<ReceiptId>,
    pub pass: Option<OpenPass>,
    pub entries: LedgerEntries,
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
    /// The question the last `needs_input` asked: the one a waiting pass waits on, or, once
    /// the lead's next turn reopened the pass, the one it asked before. `None` when the pass
    /// was never held.
    pub question: Option<String>,
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

/// A host's stop hook asking whether the session's turn may end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GateRequest {
    pub session: HookSession,
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

/// A run that may need settling, with the facts `settle` decides from and what a failure it
/// finds records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettleCandidate {
    pub run: RunId,
    /// The run's ledger sequence when read. A failure found by settling is recorded at it.
    pub sequence: Revision,
    /// The pass `facts.pass` describes: the pass an `unfinished_review` names. `None` with no
    /// pass, when an `update_failed` names only the run.
    pub pass: Option<PassId>,
    /// Findings on file, for an `unfinished_review` reason.
    pub active_findings: u32,
    pub facts: SettleFacts,
}

/// A delivery failure, recorded once per run, pass and kind: `unfinished_review` for a pass
/// found unfinished by settling, `closing_refused` for a refused closing call and its pass if
/// one is open, `update_failed` for an update of a run with no pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureRecord {
    pub run: RunId,
    pub pass: Option<PassId>,
    pub kind: FailureKind,
    /// Shown with the failed delivery: 1–2,048 bytes.
    pub reason: String,
    /// The run's ledger sequence the failure was found at. Settling leaves out a run whose
    /// unresolved failure carries the current sequence.
    pub sequence: Revision,
    /// The native turn of a refused closing call, when the host named it. `None` for a
    /// failure found by settling, which belongs to no call.
    pub turn: Option<NativeTurnId>,
}

/// The ledger's storage. Each write is one short transaction and each read one read
/// transaction; none reads a transcript or asks a provider. A tool call that cannot be
/// attributed to the run's lead or a verified child fails with `Forbidden`.
pub trait LedgerStore: Send + Sync {
    /// Applies one record write. A known call returns its stored result, or
    /// `IdempotencyConflict` when that call made another write. Checks the duplicate rules,
    /// the expected revision and the per-run limits, and returns a broken rule as a
    /// `WriteRefusal` with nothing written. Otherwise writes the record, appends its full
    /// state to the change history, advances the run's ledger sequence, opens a full-review
    /// pass when `record_opens_pass` says so and stores the result for replay. A call from the
    /// lead itself proves its turn is running, so a stored idle or failed state of the lead
    /// becomes unknown until the host reports it again. Never reads or changes a user's
    /// disposition.
    fn write_record(
        &self,
        binding: CallBinding,
        write: RecordWrite,
    ) -> AppFuture<'_, Result<Recorded, WriteRefusal>>;

    /// Reads one page of the run's ledger, holding whole records up to
    /// `MAX_LEDGER_PAGE_BYTES` and at least one. A requested ID that names no finding of the
    /// run is `InvalidFields` with one error at `/ids/N` for each, N being its index in the
    /// input; a cursor issued for another run or selection is `InvalidFields` at `/cursor`.
    /// `CursorExpired` when the ledger sequence moved since `after` was issued.
    fn read_ledger(&self, session: HostSession, read: LedgerRead) -> AppFuture<'_, LedgerPage>;

    /// Holds the run's open pass for `needs_input` until the lead's next turn starts, and
    /// returns `None` with no pass open. `Forbidden` for a child. A known call returns its
    /// stored result. Completes no assessment.
    fn hold_pass(&self, binding: CallBinding, reason: String)
    -> AppFuture<'_, Option<HoldReceipt>>;

    /// Answers one stop request: applies `gate` to the stored facts and stores the decision's
    /// block count for the turn on the pass. The session is the lead when it is a run's lead
    /// session; any other session, including one in no run, is released. A Codex request also
    /// stores that Codex's stop hook has reported, once; the hook runs for every Codex
    /// session, so a released request writes nothing once that is stored.
    fn answer_gate(&self, request: GateRequest) -> AppFuture<'_, GateAnswer>;

    /// Reads what `finish_review` composes from. A known call returns `Replayed`. A call from
    /// a run's lead returns `Current`, or `Refused` when the run's workspace, grant or
    /// settings authority or its follow-up binding changed; the caller then records
    /// `closing_refused` and refuses the call. A call from a child is `Forbidden`, and so is one attributed to no
    /// run's lead or child; neither records a failure.
    fn closing_snapshot(&self, binding: CallBinding) -> AppFuture<'_, ClosingRead>;

    /// Reads what an update composes from. `NotFound` for an unknown run, `Conflict` when it
    /// has no current workspace or grant authority.
    fn update_snapshot(&self, run: RunId) -> AppFuture<'_, LedgerSnapshot>;

    /// Commits a publish after rechecking its guard. A closing call whose call ID is already
    /// stored is `Stale`, so the next snapshot replays its acknowledgement. Otherwise the first
    /// rule that applies decides, and each but the last writes nothing:
    ///
    /// 1. A changed ledger sequence, disposition revision or head is `Stale`, even when the
    ///    pass or batch changed too, so a closing call that lost a race recomposes instead of
    ///    being refused.
    /// 2. For an update, a pass opened after the snapshot is `Stale`: the next snapshot shows
    ///    the pass, and settling then publishes nothing.
    /// 3. A changed authority, or for a closing call a changed pass or frozen batch, is
    ///    `Err(Conflict)`. The closing caller records `closing_refused`.
    /// 4. Assessments that do not cover the frozen batch are `Invalid`.
    /// 5. Otherwise, in one transaction: stores the text in `CHUNK_BYTES` chunks, appends the
    ///    version with its finding, section and outcome rows, moves the head, binds the
    ///    disposition revisions, closes the pass, completes the batch, resolves earlier
    ///    failures, stores the closing acknowledgement for replay, inserts the notification
    ///    and emits the change events. That is `Published`, or `Unchanged` for a closing
    ///    commit without a version.
    fn commit_publish(&self, commit: PublishCommit) -> AppFuture<'_, PublishOutcome>;

    /// Builds the reader's display index for a version that just committed. A failure
    /// leaves the version readable, so callers may ignore it.
    fn index_version(&self, run: RunId, receipt: ReceiptId) -> AppFuture<'_, ()>;

    /// The runs with an open pass or unpublished records, with their settle facts from
    /// stored session state, in one indexed read. Leaves out a run whose unresolved failure of
    /// the kind settling would record (`unfinished_review` for its open pass, `update_failed`
    /// when it has none) was recorded at its current ledger sequence, so it is not reworked
    /// until a record write or closing call changes it.
    fn runs_to_settle(&self) -> AppFuture<'_, Vec<SettleCandidate>>;

    /// Records a failure with one notification when first seen for its run, pass and kind.
    /// A repeat updates the reason and sequence in place and raises nothing. A failure for a
    /// pass that is already closed is stored resolved.
    fn record_failure(&self, failure: FailureRecord) -> AppFuture<'_, ()>;

    /// What the read-back shows, or `None` unless `session` is a run's lead session.
    fn read_back(&self, session: HookSession) -> AppFuture<'_, Option<ReadBack>>;

    /// Whether Codex's stop hook has ever reported on this installation. Until it has, the
    /// hook most likely still awaits the user's trust.
    fn codex_stop_hook_reported(&self) -> AppFuture<'_, bool>;
}
