//! Findings-ledger identities, states and the pure rules that decide passes, the stop gate,
//! settling and the kind of each published version.
//!
//! Rules take a small struct of facts the caller already read and return a decision. They do
//! no I/O and read no clock, so the store applies them inside its transactions and the app
//! applies them to a snapshot.

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use crate::identity::InvalidIdentity;

/// Blocks the stop gate allows in one stop sequence. The next stop request of that sequence
/// lets the turn end.
pub const MAX_GATE_BLOCKS: u8 = 2;

const MAX_SQLITE_INTEGER: u64 = i64::MAX as u64;

/// A finding's ID within its run: `F-01`, `F-02`, … `F-100`. The store assigns the next
/// unused number per run and never reuses one. IDs order by number, so `F-99` sorts before
/// `F-100`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FindingId(u32);

impl FindingId {
    /// The first ID of a run.
    pub const FIRST: Self = Self(1);

    /// The ID with this number. Fails for zero.
    pub fn new(number: u32) -> Result<Self, InvalidIdentity> {
        if number == 0 {
            return Err(InvalidIdentity);
        }
        Ok(Self(number))
    }

    pub const fn number(self) -> u32 {
        self.0
    }

    /// The ID after this one, or `None` past `u32::MAX`.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl FromStr for FindingId {
    type Err = InvalidIdentity;

    /// Accepts only the canonical form `Display` writes: `F-` and at least two digits with no
    /// extra leading zero. `F-1`, `F-00` and `F-001` fail.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let digits = value.strip_prefix("F-").ok_or(InvalidIdentity)?;
        if digits.len() < 2
            || (digits.len() > 2 && digits.starts_with('0'))
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(InvalidIdentity);
        }
        Self::new(digits.parse().map_err(|_| InvalidIdentity)?)
    }
}

impl TryFrom<&str> for FindingId {
    type Error = InvalidIdentity;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl Display for FindingId {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        write!(output, "F-{:02}", self.0)
    }
}

/// The revision of one finding or note. It starts at 1 and advances with each accepted
/// change; a write equal to the stored record leaves it unchanged.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RecordRevision(u64);

impl RecordRevision {
    /// The revision of a newly filed record.
    pub const FIRST: Self = Self(1);

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn as_sqlite_integer(self) -> i64 {
        self.0 as i64
    }

    /// The revision after this one, or `None` past SQLite's integer range.
    pub fn next(self) -> Option<Self> {
        Self::try_from(self.0.checked_add(1)?).ok()
    }
}

impl TryFrom<u64> for RecordRevision {
    type Error = InvalidIdentity;

    /// Fails for zero and for values beyond SQLite's integer range.
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if !(1..=MAX_SQLITE_INTEGER).contains(&value) {
            return Err(InvalidIdentity);
        }
        Ok(Self(value))
    }
}

impl TryFrom<i64> for RecordRevision {
    type Error = InvalidIdentity;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::try_from(u64::try_from(value).map_err(|_| InvalidIdentity)?)
    }
}

impl Display for RecordRevision {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, output)
    }
}

/// A closed set of states with one stored and wire label each.
macro_rules! labelled {
    (
        $(#[$doc:meta])*
        $name:ident { $($(#[$variant_doc:meta])* $variant:ident => $label:literal),+ $(,)? }
    ) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum $name {
            $($(#[$variant_doc])* $variant),+
        }

        impl $name {
            /// The label stored in SQLite and used on the wire.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }

            /// The value a stored label names, or `None` for any other text.
            pub fn from_label(label: &str) -> Option<Self> {
                match label {
                    $($label => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

labelled!(
    /// What an open pass owes its closing call for.
    PassKind {
        /// Started from ReviewQ, or opened by the first record of a run with no version.
        FullReview => "full_review",
        /// Opened by a delivered selected-thread batch.
        SelectedThreads => "selected_threads",
    }
);

labelled!(
    /// Where a pass stands. A run has at most one pass that is not `Closed`.
    PassState {
        /// The lead owes a closing call at the end of each of its turns.
        Open => "open",
        /// `needs_input` held the pass. It waits on the reviewer until the lead's next turn
        /// starts, and is open again from then.
        Waiting => "waiting",
        /// Closed by a publish, or ended by a successor batch or by archiving the run.
        Closed => "closed",
    }
);

labelled!(
    /// The continuity a published version establishes.
    VersionKind {
        /// A new complete-MR baseline at the stated reviewed revision.
        FullReview => "full_review",
        /// The result of one selected-thread batch, at the batch's checked SHA.
        SelectedThreads => "selected_threads",
        /// Records changed with no pass open. Carries the parent's verdict, revision and
        /// coverage unless the closing call states new ones.
        Update => "update",
    }
);

labelled!(
    /// How a finding of a version relates to the parent version.
    VersionState {
        /// Active, and the parent version lacks it.
        New => "new",
        /// On the parent, still active, at a newer revision.
        Updated => "updated",
        /// On the parent, now with a retiring outcome. It leaves the active set.
        Retired => "retired",
        /// On the parent at the same revision.
        Carried => "carried",
    }
);

labelled!(
    /// A delivery failure, recorded once per run, pass and kind.
    FailureKind {
        /// The lead's turn ended with an open pass that was not waiting on the reviewer.
        UnfinishedReview => "unfinished_review",
        /// `finish_review` was refused because the run's lead, workspace, grant, settings
        /// authority or follow-up binding changed.
        ClosingRefused => "closing_refused",
        /// An update owed with no pass open could not be published.
        UpdateFailed => "update_failed",
    }
);

labelled!(
    /// The six ledger tools.
    Tool {
        RecordFinding => "record_finding",
        AssessFinding => "assess_finding",
        RecordNote => "record_note",
        ReviewLedger => "review_ledger",
        FinishReview => "finish_review",
        NeedsInput => "needs_input",
    }
);

labelled!(
    /// Who made a ledger call, as the store attributed it.
    CallerRole {
        /// The run's registered lead itself: a Claude call without a subagent ID, or a Codex
        /// call from the lead's own thread.
        Lead => "lead",
        /// A Claude subagent of the lead, or a Codex thread that descends from the lead.
        Child => "child",
    }
);

impl PassKind {
    /// The kind of version that closes a pass of this kind on a run that already has one.
    pub const fn version_kind(self) -> VersionKind {
        match self {
            Self::FullReview => VersionKind::FullReview,
            Self::SelectedThreads => VersionKind::SelectedThreads,
        }
    }
}

/// Whether `role` may call `tool`. Only the lead closes: `finish_review` and `needs_input`
/// refuse a child.
pub fn may_call(tool: Tool, role: CallerRole) -> bool {
    match role {
        CallerRole::Lead => true,
        CallerRole::Child => match tool {
            Tool::RecordFinding | Tool::AssessFinding | Tool::RecordNote | Tool::ReviewLedger => {
                true
            }
            Tool::FinishReview | Tool::NeedsInput => false,
        },
    }
}

/// Something that may open a pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassEvent {
    /// Start confirmed a review that ReviewQ launched. A launch registers a new run, so no
    /// pass of that run is open yet.
    ReviewStarted,
    /// A selected-thread batch was delivered to the lead. The store ends the predecessor
    /// batch's pass in the same transaction; delivery never starts while a full-review pass
    /// is open.
    BatchDelivered,
    /// A record was filed or changed.
    RecordFiled {
        /// The run has a published version.
        has_version: bool,
        /// The run already has a pass that is not closed.
        pass_open: bool,
    },
}

/// The pass `event` opens, if any. A record opens a full-review pass only on a run with no
/// version and no open pass. A message the reviewer sends, report feedback and a publication
/// action open nothing and are not events here.
pub fn opens_pass(event: PassEvent) -> Option<PassKind> {
    match event {
        PassEvent::ReviewStarted => Some(PassKind::FullReview),
        PassEvent::BatchDelivered => Some(PassKind::SelectedThreads),
        PassEvent::RecordFiled {
            has_version,
            pass_open,
        } => (!has_version && !pass_open).then_some(PassKind::FullReview),
    }
}

/// What the stop gate knows about one stop request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GateFacts {
    /// Who is stopping.
    pub role: CallerRole,
    /// The state of the run's pass, or `None` with no pass. `Closed` counts as no pass.
    pub pass: Option<PassState>,
    /// A blocking question or permission request is pending in the session.
    pub pending_question: bool,
    /// The host's `stopHookActive`: this stop follows a block. `false` starts a new sequence.
    pub stop_hook_active: bool,
    /// Blocks already recorded for this turn's current stop sequence; 0 when the stored count
    /// belongs to another turn.
    pub blocks: u8,
}

/// The gate's answer and the block count the store records for the sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateDecision {
    /// Let the turn end.
    Release {
        /// The sequence's count to store: 0 when this request started a new sequence.
        blocks: u8,
    },
    /// Ask the lead for its closing call before the turn ends.
    Block {
        /// The sequence's count to store, including this block.
        blocks: u8,
    },
}

/// Blocks only a lead's stop while its pass is open and not waiting, no blocking question or
/// permission is pending, and fewer than `MAX_GATE_BLOCKS` blocks were given in this stop
/// sequence. A stop with `stop_hook_active` false starts a new sequence at 0. Never blocks a
/// child, a turn that owes nothing, or a turn in which `needs_input` held the pass.
pub fn gate(facts: GateFacts) -> GateDecision {
    let earlier = if facts.stop_hook_active {
        facts.blocks
    } else {
        0
    };
    let owes = match (facts.role, facts.pass) {
        (CallerRole::Lead, Some(PassState::Open)) => !facts.pending_question,
        (CallerRole::Lead, Some(PassState::Waiting | PassState::Closed) | None)
        | (CallerRole::Child, _) => false,
    };
    if owes && earlier < MAX_GATE_BLOCKS {
        GateDecision::Block {
            blocks: earlier + 1,
        }
    } else {
        GateDecision::Release { blocks: earlier }
    }
}

/// What settling knows about one run, read from stored session state. No provider is asked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettleFacts {
    pub lead_idle: bool,
    /// Every child the run knows of is idle.
    pub children_idle: bool,
    pub recovery_pending: bool,
    /// The state of the run's pass, or `None` with no pass. `Closed` counts as no pass.
    pub pass: Option<PassState>,
    /// Some record changed after the run's current version. A run without a version always
    /// has a pass while it has records, because its first record opened one.
    pub unpublished: bool,
}

/// What settling does for a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Settlement {
    Nothing,
    /// Publish an update that carries the parent's verdict, revision and coverage.
    PublishUpdate,
    /// Record `unfinished_review` for the open pass.
    Unfinished,
}

/// A run is settled when its lead and every known child are idle and no recovery is pending.
/// A settled run with an open pass that is not waiting is unfinished. A settled run with no
/// pass and unpublished records gets an update. Anything else needs nothing yet.
pub fn settle(facts: SettleFacts) -> Settlement {
    if !facts.lead_idle || !facts.children_idle || facts.recovery_pending {
        return Settlement::Nothing;
    }
    match facts.pass {
        Some(PassState::Open) => Settlement::Unfinished,
        Some(PassState::Waiting) => Settlement::Nothing,
        Some(PassState::Closed) | None if facts.unpublished => Settlement::PublishUpdate,
        Some(PassState::Closed) | None => Settlement::Nothing,
    }
}

/// What a `finish_review` call states and finds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClosingFacts {
    /// The run has a published version.
    pub has_version: bool,
    /// The kind of the run's open or waiting pass. A waiting pass can still be closed.
    pub pass: Option<PassKind>,
    /// The call gave a verdict.
    pub verdict: bool,
    /// The call gave a revision.
    pub revision: bool,
    /// The call set `fullReview` to true.
    pub full_review: bool,
    /// The call gave at least one assessment.
    pub assessments: bool,
}

/// A closing call the lead corrects in its turn. Each names one input field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosingError {
    /// `/fullReview`: a selected-thread pass cannot close as a full review.
    FullReviewInSelectedThreads,
    /// `/assessments`: assessments belong to a selected-thread pass.
    AssessmentsWithoutSelectedThreads,
    /// `/verdict`: a full review, which includes every first version, needs a verdict.
    MissingVerdict,
    /// `/revision`: a full review, which includes every first version, needs a revision.
    MissingRevision,
}

/// The kind of version a closing call publishes, or every error in it. A run with no version
/// gets a full review whatever pass is open, so a review with no findings can publish. Then
/// an open pass decides. With no pass, `fullReview` asks for a full review and otherwise the
/// call publishes an update.
pub fn closing_kind(facts: ClosingFacts) -> Result<VersionKind, Vec<ClosingError>> {
    let selected = match facts.pass {
        Some(PassKind::SelectedThreads) => true,
        Some(PassKind::FullReview) | None => false,
    };
    let mut errors = Vec::new();
    if selected && facts.full_review {
        errors.push(ClosingError::FullReviewInSelectedThreads);
    }
    if !selected && facts.assessments {
        errors.push(ClosingError::AssessmentsWithoutSelectedThreads);
    }
    let kind = match (facts.has_version, facts.pass) {
        (false, _) => VersionKind::FullReview,
        (true, Some(pass)) => pass.version_kind(),
        (true, None) if facts.full_review => VersionKind::FullReview,
        (true, None) => VersionKind::Update,
    };
    if kind == VersionKind::FullReview {
        if !facts.verdict {
            errors.push(ClosingError::MissingVerdict);
        }
        if !facts.revision {
            errors.push(ClosingError::MissingRevision);
        }
    }
    if errors.is_empty() {
        Ok(kind)
    } else {
        Err(errors)
    }
}

/// The active findings of the parent version that `touched` lacks, in `parent_active` order.
/// A full review refuses to publish while any remain: the lead changes or assesses each.
pub fn untouched_findings(parent_active: &[FindingId], touched: &[FindingId]) -> Vec<FindingId> {
    parent_active
        .iter()
        .copied()
        .filter(|id| !touched.contains(id))
        .collect()
}

/// How a finding relates to the parent version, from its revision on the parent's active set
/// (`None` when the parent lacks it), its current revision and whether its current outcome
/// retires it. A retiring outcome decides first. `None` means the version leaves the finding
/// out: it was retired before any version showed it, or retired on an earlier version.
pub fn version_state(
    parent_revision: Option<RecordRevision>,
    current_revision: RecordRevision,
    retired: bool,
) -> Option<VersionState> {
    match (parent_revision, retired) {
        (None, true) => None,
        (None, false) => Some(VersionState::New),
        (Some(_), true) => Some(VersionState::Retired),
        (Some(parent), false) if parent == current_revision => Some(VersionState::Carried),
        (Some(_), false) => Some(VersionState::Updated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settle_and_gate_decisions() {
        let settled = SettleFacts {
            lead_idle: true,
            children_idle: true,
            recovery_pending: false,
            pass: None,
            unpublished: false,
        };
        let settle_cases = [
            (
                "a child still working",
                SettleFacts {
                    children_idle: false,
                    pass: Some(PassState::Open),
                    ..settled
                },
                Settlement::Nothing,
            ),
            (
                "a waiting pass",
                SettleFacts {
                    pass: Some(PassState::Waiting),
                    unpublished: true,
                    ..settled
                },
                Settlement::Nothing,
            ),
            (
                "a pending recovery",
                SettleFacts {
                    recovery_pending: true,
                    pass: Some(PassState::Open),
                    ..settled
                },
                Settlement::Nothing,
            ),
            (
                "unpublished records with no pass",
                SettleFacts {
                    unpublished: true,
                    ..settled
                },
                Settlement::PublishUpdate,
            ),
            (
                "an open pass at rest",
                SettleFacts {
                    pass: Some(PassState::Open),
                    unpublished: true,
                    ..settled
                },
                Settlement::Unfinished,
            ),
        ];
        for (case, facts, expected) in settle_cases {
            assert_eq!(settle(facts), expected, "{case}");
        }

        // Each stop request feeds back the count the previous answer told the store to keep.
        let lead = |pass, stop_hook_active, blocks| GateFacts {
            role: CallerRole::Lead,
            pass: Some(pass),
            pending_question: false,
            stop_hook_active,
            blocks,
        };
        let gate_cases = [
            (
                "first stop blocks",
                lead(PassState::Open, false, 0),
                GateDecision::Block { blocks: 1 },
            ),
            (
                "second stop blocks",
                lead(PassState::Open, true, 1),
                GateDecision::Block { blocks: 2 },
            ),
            (
                "third stop releases",
                lead(PassState::Open, true, 2),
                GateDecision::Release { blocks: 2 },
            ),
            (
                "a fresh stop starts again",
                lead(PassState::Open, false, 2),
                GateDecision::Block { blocks: 1 },
            ),
            (
                "a waiting pass is never blocked",
                lead(PassState::Waiting, false, 0),
                GateDecision::Release { blocks: 0 },
            ),
        ];
        for (case, facts, expected) in gate_cases {
            assert_eq!(gate(facts), expected, "{case}");
        }
    }
}
