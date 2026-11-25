//! The fixed words of the composed report, as the spec's composed layout writes them: the
//! eight section headings with the line an empty one shows, the four cover labels and the
//! card's field labels. The composer writes them and the field checks refuse authored text
//! that a reader could take for them.

use reviewq_domain::ledger::VersionKind;

use crate::reports::{FindingConclusion, Verdict};

/// One of the eight sections every version shows, in layout order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FixedSection {
    Flow,
    Changes,
    Threads,
    Functional,
    Quality,
    Questions,
    Verification,
    Publication,
}

impl FixedSection {
    /// Every fixed section, in the order the report shows them. Extra notes follow
    /// `Verification`.
    pub(crate) const ALL: [Self; 8] = [
        Self::Flow,
        Self::Changes,
        Self::Threads,
        Self::Functional,
        Self::Quality,
        Self::Questions,
        Self::Verification,
        Self::Publication,
    ];

    /// The level-two heading's text, without its `## ` marks.
    pub(crate) const fn heading(self) -> &'static str {
        match self {
            Self::Flow => "Principal code / data flow",
            Self::Changes => "What changes across the MR",
            Self::Threads => "Our previous threads",
            Self::Functional => "Functional findings",
            Self::Quality => "Quality findings",
            Self::Questions => "Open questions and decisions",
            Self::Verification => "Verification and limits",
            Self::Publication => "Publication and dispositions",
        }
    }

    /// The line the section shows when it has nothing to hold.
    pub(crate) const fn empty_line(self) -> &'static str {
        match self {
            Self::Flow | Self::Changes | Self::Verification => "Not recorded.",
            Self::Threads => "No earlier threads.",
            Self::Functional => "No functional findings.",
            Self::Quality => "No quality findings.",
            Self::Questions => "No open questions.",
            Self::Publication => "No publication or disposition changes recorded.",
        }
    }
}

/// The bold labels of the four cover lines, colon included, in layout order.
pub(crate) const COVER_LABELS: [&str; 4] = [
    "Scope:",
    "Reviewed source:",
    "Reviewed target:",
    "Checked at:",
];

/// A card's bold field label, colon included, as the composer writes it before the field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CardLabel {
    Location,
    Issue,
    WhyItMatters,
    /// Before observed evidence.
    Confirmed,
    /// Before a gap, whose text the composer starts with `Not confirmed.`
    Evidence,
    ProposedComment,
}

impl CardLabel {
    /// Every card label, in the order a card shows its fields. `Confirmed` and `Evidence`
    /// share one place: a card shows one of them.
    pub(crate) const ALL: [Self; 6] = [
        Self::Location,
        Self::Issue,
        Self::WhyItMatters,
        Self::Confirmed,
        Self::Evidence,
        Self::ProposedComment,
    ];

    pub(crate) const fn text(self) -> &'static str {
        match self {
            Self::Location => "Location:",
            Self::Issue => "Issue:",
            Self::WhyItMatters => "Why it matters:",
            Self::Confirmed => "Confirmed:",
            Self::Evidence => "Evidence:",
            Self::ProposedComment => "Proposed comment:",
        }
    }
}

/// Whether an `extra` note's `title` repeats a fixed section's heading. The composer writes
/// the title as a level-two heading, whose text Markdown trims, and a heading that differs
/// only in letter case still reads as the same section, so the comparison trims the title
/// and ignores case. The headings are ASCII, so ASCII case folding is exact.
pub(crate) fn repeats_fixed_heading(title: &str) -> bool {
    let title = title.trim();
    FixedSection::ALL
        .iter()
        .any(|section| section.heading().eq_ignore_ascii_case(title))
}

/// Between the parts of the identity line, a card's heading, the card line, the counts line,
/// a previous-threads cell and a disposition line.
pub(crate) const SEPARATOR: &str = " · ";

/// The verdict line's text, which the composer writes in bold. `None` for `not_specified`,
/// which leaves the line out.
pub(crate) const fn verdict_line(verdict: Verdict) -> Option<&'static str> {
    match verdict {
        Verdict::Approve => Some("APPROVE"),
        Verdict::RequestChanges => Some("REQUEST CHANGES"),
        Verdict::Blocked => Some("BLOCKED"),
        Verdict::NotSpecified => None,
    }
}

/// The `Scope:` cover line's value for each kind of version.
pub(crate) const fn scope(kind: VersionKind) -> &'static str {
    match kind {
        VersionKind::FullReview => "Full review",
        VersionKind::SelectedThreads => "Selected threads",
        VersionKind::Update => "Update",
    }
}

/// The word for an effective blocking value on the card line, in the counts line and in a
/// disposition line.
pub(crate) const fn blocking_word(blocking: bool) -> &'static str {
    if blocking { "Blocking" } else { "Nonblocking" }
}

/// What the cover's reviewed source or target says when ReviewQ does not know the SHA, and
/// what a previous-threads row says for an unknown checked revision.
pub(crate) const UNKNOWN_REVISION: &str = "unknown";

/// The first words of gap evidence, written after the `Evidence:` label.
pub(crate) const GAP_OPENING: &str = "Not confirmed.";

/// The header cells of the previous-threads table, in column order.
pub(crate) const THREADS_HEADER: [&str; 3] =
    ["Finding", "Outcome and verification", "Thread or evidence"];

/// The outcome a previous-threads row states in bold before its reason.
pub(crate) const fn outcome_label(conclusion: FindingConclusion) -> &'static str {
    match conclusion {
        FindingConclusion::Fixed => "Fixed",
        FindingConclusion::ClarifiedWithdrawn => "Clarified and withdrawn",
        FindingConclusion::Superseded => "Superseded",
        FindingConclusion::PartlyAddressed => "Partly addressed",
        FindingConclusion::StillPresent => "Still present",
        FindingConclusion::Unverified => "Unverified",
    }
}

/// A previous-threads row's last cell when the outcome links no discussion.
pub(crate) const NO_LINKED_DISCUSSION: &str = "No linked discussion";

/// Before the checked revision in a previous-threads row's last cell.
pub(crate) const CHECKED_REVISION_LABEL: &str = "Checked revision:";

/// Before a disposition's publication choice in a `Publication and dispositions` line.
pub(crate) const PUBLICATION_LABEL: &str = "Publication:";

/// Before a finding's monitored discussions in a `Publication and dispositions` line.
pub(crate) const DISCUSSIONS_LABEL: &str = "Discussions:";
