//! The fixed words of the composed report, as the spec's composed layout writes them: the
//! seven section headings with the line an empty one shows, the four cover labels, the counts
//! line and the card's field labels. The composer writes them and the field checks refuse
//! authored text that a reader could take for them.

use std::fmt::{self, Display, Formatter};

use reviewq_domain::ledger::VersionKind;

use crate::reports::{FindingConclusion, Severity, Verdict};

/// One of the seven sections every version shows, in layout order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FixedSection {
    Flow,
    Changes,
    Threads,
    Findings,
    Questions,
    Verification,
    Publication,
}

impl FixedSection {
    /// Every fixed section, in the order the report shows them. Extra notes follow
    /// `Verification`.
    pub(crate) const ALL: [Self; 7] = [
        Self::Flow,
        Self::Changes,
        Self::Threads,
        Self::Findings,
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
            Self::Findings => "Findings",
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
            Self::Findings => "No findings.",
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
    /// A line of its own above the quoted comment: `PROPOSED_COMMENT_OPENING`, where the
    /// comment would go, then `:`.
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

    /// The label as an error message names it. `ProposedComment` stands for every lead line
    /// with `…`.
    pub(crate) const fn text(self) -> &'static str {
        match self {
            Self::Location => "Location:",
            Self::Issue => "Issue:",
            Self::WhyItMatters => "Why it matters:",
            Self::Confirmed => "Confirmed:",
            Self::Evidence => "Evidence:",
            Self::ProposedComment => "Proposed comment — …:",
        }
    }

    /// Whether a paragraph's leading bold text, `bold`, reads as this label, as the reader
    /// matches it: the whole text, or for `ProposedComment` any text that starts with
    /// `PROPOSED_COMMENT_OPENING` and ends with `:`.
    pub(crate) fn matches(self, bold: &str) -> bool {
        match self {
            Self::ProposedComment => {
                bold.starts_with(PROPOSED_COMMENT_OPENING) && bold.ends_with(':')
            }
            Self::Location
            | Self::Issue
            | Self::WhyItMatters
            | Self::Confirmed
            | Self::Evidence => bold == self.text(),
        }
    }
}

/// The start of the proposed comment's bold lead line.
pub(crate) const PROPOSED_COMMENT_OPENING: &str = "Proposed comment — ";

/// Where a proposed comment would go, after `PROPOSED_COMMENT_OPENING`: a reply to the note
/// the finding names or in its linked thread, a new comment at its first location, or a new
/// comment on the MR.
pub(crate) const REPLY_IN: &str = "reply in ";
pub(crate) const REPLY_THREAD: &str = "thread";
pub(crate) const NEW_COMMENT_AT: &str = "new comment at ";
pub(crate) const NEW_COMMENT_ON_MR: &str = "new comment on the MR";

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

/// The counts line: `P0: {n} · P1: {n} · P2: {n} · P3: {n} · Blocking: {n} · Nonblocking: {n}`,
/// the active findings by severity and by effective blocking value. `Display` writes it, and
/// `matches` recognises text of its shape, so the field checks refuse a paragraph the reader
/// would take for it.
pub(crate) struct CountsLine {
    /// Active findings at P0, P1, P2 and P3.
    pub(crate) severities: [u32; 4],
    pub(crate) blocking: u32,
    pub(crate) nonblocking: u32,
}

impl CountsLine {
    /// The label before each count, in the line's order.
    fn labels() -> impl Iterator<Item = String> {
        [Severity::P0, Severity::P1, Severity::P2, Severity::P3]
            .into_iter()
            .map(|severity| severity.to_string())
            .chain([true, false].map(|blocking| blocking_word(blocking).to_owned()))
    }

    /// Whether `text` reads as a counts line: every label in order, each followed by `: ` and
    /// one or more ASCII digits, joined by `SEPARATOR`, with nothing before or after.
    pub(crate) fn matches(text: &str) -> bool {
        let mut rest = text;
        for (index, label) in Self::labels().enumerate() {
            if index > 0 {
                let Some(after) = rest.strip_prefix(SEPARATOR) else {
                    return false;
                };
                rest = after;
            }
            let Some(after) = rest
                .strip_prefix(label.as_str())
                .and_then(|after| after.strip_prefix(": "))
            else {
                return false;
            };
            let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            if digits == 0 {
                return false;
            }
            rest = &after[digits..];
        }
        rest.is_empty()
    }
}

impl Display for CountsLine {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        let [p0, p1, p2, p3] = self.severities;
        let counts = [p0, p1, p2, p3, self.blocking, self.nonblocking];
        for (index, (label, count)) in Self::labels().zip(counts).enumerate() {
            if index > 0 {
                output.write_str(SEPARATOR)?;
            }
            write!(output, "{label}: {count}")?;
        }
        Ok(())
    }
}

/// What the cover's reviewed source or target says when ReviewQ does not know the SHA.
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

/// The text of a previous-threads link to a discussion's note, numbered from 1 after a space
/// when the row links several.
pub(crate) const THREAD_LINK: &str = "Thread";

/// How many characters of the checked revision's SHA its previous-threads link shows.
pub(crate) const SHORT_SHA_CHARS: usize = 8;

/// A previous-threads row's last cell when the outcome links no discussion and its checked
/// revision is unknown.
pub(crate) const NO_THREAD_OR_EVIDENCE: &str = "—";

/// Before a disposition's publication choice in a `Publication and dispositions` line.
pub(crate) const PUBLICATION_LABEL: &str = "Publication:";

/// Before a finding's monitored discussions in a `Publication and dispositions` line.
pub(crate) const DISCUSSIONS_LABEL: &str = "Discussions:";
