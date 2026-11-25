//! Composes a version's text from a snapshot's cover facts, findings and notes and the
//! version's own facts, in one pass that records each card's and section's span as it
//! writes. It parses nothing: fields were checked when filed, and their headings are shifted
//! with `markdown::shift_headings`. A span runs from its heading to the end of its last line,
//! without the line ending; a section's span holds its cards.

mod output;
#[cfg(test)]
mod tests;

use std::fmt::{self, Write};

use reviewq_domain::identity::GitSha;
use reviewq_domain::ledger::{VersionState, version_state};

use super::layout::{
    CHECKED_REVISION_LABEL, COVER_LABELS, CardLabel, DISCUSSIONS_LABEL, FixedSection, GAP_OPENING,
    NO_LINKED_DISCUSSION, PUBLICATION_LABEL, SEPARATOR, THREADS_HEADER, UNKNOWN_REVISION,
    blocking_word, outcome_label, scope, verdict_line,
};
use super::markdown::shift_headings;
use super::{
    Composed, CoverFacts, FindingFields, FindingOutcome, NoteRecord, NoteSection, SnapshotFinding,
    VersionFacts, default_category,
};
use crate::reports::{FindingBasisKind, FindingSection, RevisionEvidence, Severity};
use output::{
    Cell, Output, UtcSeconds, without_blank_lines, write_code, write_heading_text, write_literal,
};

/// Bytes reserved per card or note beyond its record's own text: labels, the card line and
/// the blank lines between blocks.
const BLOCK_OVERHEAD_BYTES: usize = 256;
/// Bytes reserved for the identity line, the verdict, counts and cover lines, the fixed
/// headings and the lines of empty sections.
const FRAME_BYTES: usize = 2048;

/// Composes the version `facts` describes from the snapshot's cover facts, findings (in ID
/// order) and notes (fixed sections in layout order, then extra notes in the order first
/// filed). Returns the text with one span per active finding's card and per section, its
/// SHA-256 and its line statistics. Never fails: every field was checked when it was filed.
pub(super) fn compose(
    cover: &CoverFacts,
    facts: &VersionFacts,
    findings: &[SnapshotFinding],
    notes: &[NoteRecord],
) -> Composed {
    let report = Report {
        cover,
        facts,
        findings,
        notes,
    };
    let mut out = Output::with_capacity(report.reserve());
    report
        .write(&mut out)
        .expect("composing into a String cannot fail");
    out.finish()
}

/// How the finding relates to the parent version, or `None` when the version leaves it out.
pub(super) fn state(finding: &SnapshotFinding) -> Option<VersionState> {
    version_state(
        finding.parent.map(|parent| parent.revision),
        finding.record.revision,
        !finding.record.is_active(),
    )
}

/// The outcome this version records for `finding`: one recorded since the parent version,
/// on a finding the version shows or retires. Each has a row under `Our previous threads`
/// and an entry in the version's changes.
pub(super) fn recorded_outcome(finding: &SnapshotFinding) -> Option<&FindingOutcome> {
    let outcome = finding
        .record
        .outcome
        .as_ref()
        .filter(|_| finding.outcome_is_new)?;
    state(finding).map(|_| outcome)
}

/// An outcome's checked revision: its own `checkedSha`, otherwise the version's exact
/// reviewed SHA, otherwise unknown.
pub(super) fn checked_revision<'a>(
    outcome: &'a FindingOutcome,
    reviewed: &'a RevisionEvidence,
) -> Option<&'a GitSha> {
    outcome.checked_sha.as_ref().or(match reviewed {
        RevisionEvidence::Exact(sha) => Some(sha),
        RevisionEvidence::Unknown(_) => None,
    })
}

/// The UTF-8 bytes of a finding's authored text, the measure a size refusal ranks by.
pub(super) fn finding_bytes(fields: &FindingFields) -> usize {
    let body = &fields.body;
    fields.title.len()
        + fields.category.as_ref().map_or(0, String::len)
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

/// The effective blocking value at publish: the user's disposition when it sets one,
/// otherwise the lead's.
fn effective_blocking(finding: &SnapshotFinding) -> bool {
    finding
        .disposition
        .as_ref()
        .and_then(|disposition| disposition.blocking)
        .unwrap_or(finding.record.fields.blocking)
}

struct Report<'a> {
    cover: &'a CoverFacts,
    facts: &'a VersionFacts,
    findings: &'a [SnapshotFinding],
    notes: &'a [NoteRecord],
}

impl Report<'_> {
    /// An upper estimate of the text's size, so the output is allocated once.
    fn reserve(&self) -> usize {
        let findings: usize = self
            .findings
            .iter()
            .map(|finding| finding_bytes(&finding.record.fields) + BLOCK_OVERHEAD_BYTES)
            .sum();
        let notes: usize = self
            .notes
            .iter()
            .map(|note| {
                note.body.len() + note.key.title().map_or(0, str::len) + BLOCK_OVERHEAD_BYTES
            })
            .sum();
        let cover = self.cover;
        FRAME_BYTES
            + cover.project_path.len()
            + cover.mr_url.len()
            + cover.mr_title.len()
            + findings
            + notes
    }

    fn write(&self, out: &mut Output) -> fmt::Result {
        self.header(out)?;
        for section in FixedSection::ALL {
            match section {
                FixedSection::Flow => self.note_section(out, section, NoteSection::Flow),
                FixedSection::Changes => self.note_section(out, section, NoteSection::Changes),
                FixedSection::Threads => self.threads(out),
                FixedSection::Functional => {
                    self.findings_section(out, section, FindingSection::Functional)
                }
                FixedSection::Quality => {
                    self.findings_section(out, section, FindingSection::Quality)
                }
                FixedSection::Questions => self.note_section(out, section, NoteSection::Questions),
                FixedSection::Verification => {
                    self.note_section(out, section, NoteSection::Verification)?;
                    self.extra_sections(out)
                }
                FixedSection::Publication => self.dispositions(out),
            }?;
        }
        out.write_char('\n')
    }

    fn note(&self, section: NoteSection) -> Option<&NoteRecord> {
        self.notes.iter().find(|note| note.key.section() == section)
    }

    fn active(&self) -> impl Iterator<Item = &SnapshotFinding> {
        self.findings
            .iter()
            .filter(|finding| finding.record.is_active())
    }

    /// The identity line, the title, the summary, the verdict and rationale, the counts line
    /// and the four cover lines.
    fn header(&self, out: &mut Output) -> fmt::Result {
        let cover = self.cover;
        out.block()?;
        write!(
            out,
            "[{}{SEPARATOR}!{}]({})",
            cover.project_path, cover.iid, cover.mr_url
        )?;
        out.block()?;
        out.write_str("# ")?;
        let title = self
            .note(NoteSection::Title)
            .map_or(cover.mr_title.as_str(), |note| note.body.as_str());
        write_heading_text(out, title)?;
        if let Some(summary) = self.note(NoteSection::Summary) {
            out.block()?;
            out.authored(&summary.body)?;
        }
        if let Some(line) = verdict_line(self.facts.verdict) {
            out.block()?;
            write!(out, "**{line}**")?;
        }
        if let Some(rationale) = self.note(NoteSection::Rationale) {
            out.block()?;
            out.authored(&rationale.body)?;
        }
        self.counts(out)?;
        self.cover_lines(out)
    }

    fn counts(&self, out: &mut Output) -> fmt::Result {
        let mut severities = [0_u32; 4];
        let mut blocking = 0_u32;
        let mut total = 0_u32;
        for finding in self.active() {
            let index = match finding.record.fields.severity {
                Severity::P0 => 0,
                Severity::P1 => 1,
                Severity::P2 => 2,
                Severity::P3 => 3,
            };
            severities[index] += 1;
            blocking += u32::from(effective_blocking(finding));
            total += 1;
        }
        out.block()?;
        for (severity, count) in [Severity::P0, Severity::P1, Severity::P2, Severity::P3]
            .into_iter()
            .zip(severities)
        {
            write!(out, "{severity}: {count}{SEPARATOR}")?;
        }
        write!(
            out,
            "{}: {blocking}{SEPARATOR}{}: {}",
            blocking_word(true),
            blocking_word(false),
            total - blocking
        )
    }

    fn cover_lines(&self, out: &mut Output) -> fmt::Result {
        let facts = self.facts;
        let [scope_label, source_label, target_label, checked_label] = COVER_LABELS;
        out.block()?;
        write!(out, "**{scope_label}** {}", scope(facts.kind))?;
        out.block()?;
        write!(out, "**{source_label}** ")?;
        match &facts.reviewed {
            RevisionEvidence::Exact(sha) => write!(out, "{sha}")?,
            RevisionEvidence::Unknown(reason) if reason.is_empty() => {
                out.write_str(UNKNOWN_REVISION)?;
            }
            RevisionEvidence::Unknown(reason) => write!(out, "{UNKNOWN_REVISION}: {reason}")?,
        }
        out.block()?;
        write!(out, "**{target_label}** ")?;
        match &facts.coverage_target_sha {
            Some(sha) => write!(out, "{sha}")?,
            None => out.write_str(UNKNOWN_REVISION)?,
        }
        out.block()?;
        write!(
            out,
            "**{checked_label}** {}",
            UtcSeconds(facts.checked_at_ms)
        )
    }

    /// A fixed section that a note fills: `flow`, `changes`, `questions` or `verification`.
    fn note_section(
        &self,
        out: &mut Output,
        section: FixedSection,
        note: NoteSection,
    ) -> fmt::Result {
        let start = out.open_section(section)?;
        if let Some(note) = self.note(note) {
            out.block()?;
            out.authored(&shift_headings(&note.body))?;
        }
        out.close_fixed_section(section, start)
    }

    /// `Our previous threads`: one table row per outcome this version records, then the
    /// `threads` note.
    fn threads(&self, out: &mut Output) -> fmt::Result {
        let start = out.open_section(FixedSection::Threads)?;
        let mut rows = self
            .findings
            .iter()
            .filter_map(|finding| recorded_outcome(finding).map(|outcome| (finding, outcome)))
            .peekable();
        if rows.peek().is_some() {
            out.block()?;
            for cell in THREADS_HEADER {
                write!(out, "| {cell} ")?;
            }
            out.write_str("|\n")?;
            for _ in THREADS_HEADER {
                out.write_str("| --- ")?;
            }
            out.write_char('|')?;
            for (finding, outcome) in rows {
                self.threads_row(out, finding, outcome)?;
            }
        }
        if let Some(note) = self.note(NoteSection::Threads) {
            out.block()?;
            out.authored(&shift_headings(&note.body))?;
        }
        out.close_fixed_section(FixedSection::Threads, start)
    }

    fn threads_row(
        &self,
        out: &mut Output,
        finding: &SnapshotFinding,
        outcome: &FindingOutcome,
    ) -> fmt::Result {
        write!(out, "\n| {}{SEPARATOR}", finding.record.id)?;
        Cell(out).write_str(&finding.record.fields.title)?;
        write!(out, " | **{}.** ", outcome_label(outcome.conclusion))?;
        Cell(out).write_str(&outcome.reason)?;
        out.write_str(" | ")?;
        let mut cell = Cell(out);
        if outcome.discussion_ids.is_empty() {
            cell.write_str(NO_LINKED_DISCUSSION)?;
        }
        for (index, discussion) in outcome.discussion_ids.iter().enumerate() {
            if index > 0 {
                cell.write_str(", ")?;
            }
            write_code(&mut cell, discussion, None)?;
        }
        write!(cell, "{SEPARATOR}{CHECKED_REVISION_LABEL} ")?;
        match checked_revision(outcome, &self.facts.reviewed) {
            Some(sha) => write_code(&mut cell, sha.as_str(), None)?,
            None => cell.write_str(UNKNOWN_REVISION)?,
        }
        out.write_str(" |")
    }

    /// `Functional findings` or `Quality findings`: the active findings of `kind`, in ID
    /// order.
    fn findings_section(
        &self,
        out: &mut Output,
        section: FixedSection,
        kind: FindingSection,
    ) -> fmt::Result {
        let start = out.open_section(section)?;
        for finding in self
            .active()
            .filter(|finding| finding.record.fields.section == kind)
        {
            card(out, finding)?;
        }
        out.close_fixed_section(section, start)
    }

    /// Each extra note as its own level-two section, in the order first filed.
    fn extra_sections(&self, out: &mut Output) -> fmt::Result {
        for note in self.notes {
            let Some(title) = note.key.title() else {
                continue;
            };
            out.block()?;
            let start = out.mark();
            out.write_str("## ")?;
            write_literal(out, title)?;
            out.block()?;
            out.authored(&shift_headings(&note.body))?;
            out.close_section(title.to_owned(), start);
        }
        Ok(())
    }

    /// `Publication and dispositions`: one line per active finding with a user disposition
    /// or a monitored discussion link, then the `publication` note.
    fn dispositions(&self, out: &mut Output) -> fmt::Result {
        let section = FixedSection::Publication;
        let start = out.open_section(section)?;
        let mut listed = false;
        for finding in self
            .active()
            .filter(|finding| finding.disposition.is_some() || !finding.discussion_ids.is_empty())
        {
            if listed {
                out.write_char('\n')?;
            } else {
                out.block()?;
                listed = true;
            }
            disposition_line(out, finding)?;
        }
        if let Some(note) = self.note(NoteSection::Publication) {
            out.block()?;
            out.authored(&shift_headings(&note.body))?;
        }
        out.close_fixed_section(section, start)
    }
}

/// One card: the heading, the card line, then each given field in the reader's order.
fn card(out: &mut Output, finding: &SnapshotFinding) -> fmt::Result {
    let record = &finding.record;
    let fields = &record.fields;
    let body = &fields.body;
    out.block()?;
    let start = out.mark();
    write!(out, "### {}{SEPARATOR}", record.id)?;
    write_heading_text(out, &fields.title)?;
    out.block()?;
    write!(out, "**{}{SEPARATOR}", fields.severity)?;
    match &fields.category {
        Some(category) => write_literal(out, category)?,
        None => out.write_str(default_category(fields.section))?,
    }
    write!(
        out,
        "{SEPARATOR}{}**",
        blocking_word(effective_blocking(finding))
    )?;
    if !body.locations.is_empty() {
        out.block()?;
        write!(out, "**{}** ", CardLabel::Location.text())?;
        for (index, location) in body.locations.iter().enumerate() {
            if index > 0 {
                out.write_str(", ")?;
            }
            write_code(out, &location.path, location.line)?;
        }
    }
    labelled_field(out, CardLabel::Issue, &body.issue)?;
    labelled_field(out, CardLabel::WhyItMatters, &body.consequence)?;
    match body.evidence.kind {
        FindingBasisKind::Observed => {
            labelled_field(out, CardLabel::Confirmed, &body.evidence.text)?;
        }
        FindingBasisKind::Gap => gap_field(out, &body.evidence.text)?,
    }
    if let Some(details) = &body.details {
        out.block()?;
        out.authored(&shift_headings(details))?;
    }
    if let Some(comment) = &body.proposed_comment {
        labelled_field(out, CardLabel::ProposedComment, comment)?;
    }
    out.close_card(record.id, start);
    Ok(())
}

/// A field whose first paragraph follows its bold label on the same line.
fn labelled_field(out: &mut Output, label: CardLabel, text: &str) -> fmt::Result {
    out.block()?;
    write!(out, "**{}** ", label.text())?;
    out.authored(&shift_headings(text))
}

/// Gap evidence under `Evidence`, beginning `Not confirmed.` unless the agent began it so.
fn gap_field(out: &mut Output, text: &str) -> fmt::Result {
    out.block()?;
    write!(out, "**{}** ", CardLabel::Evidence.text())?;
    let shifted = shift_headings(text);
    let text = without_blank_lines(&shifted);
    if !text.starts_with(GAP_OPENING) {
        write!(out, "{GAP_OPENING} ")?;
    }
    out.write_str(text)
}

/// `- F-02 · Nonblocking · Publication: withheld · Discussions: `D7` · {reason}`, with each
/// part present only when the disposition or the links give it. The user's reason is written
/// on one line, so it cannot open a block.
fn disposition_line(out: &mut Output, finding: &SnapshotFinding) -> fmt::Result {
    write!(out, "- {}", finding.record.id)?;
    let disposition = finding.disposition.as_ref();
    if let Some(blocking) = disposition.and_then(|disposition| disposition.blocking) {
        write!(out, "{SEPARATOR}{}", blocking_word(blocking))?;
    }
    if let Some(publication) = disposition.and_then(|disposition| disposition.publication) {
        write!(out, "{SEPARATOR}{PUBLICATION_LABEL} {publication}")?;
    }
    if !finding.discussion_ids.is_empty() {
        write!(out, "{SEPARATOR}{DISCUSSIONS_LABEL} ")?;
        for (index, discussion) in finding.discussion_ids.iter().enumerate() {
            if index > 0 {
                out.write_str(", ")?;
            }
            write_code(out, discussion, None)?;
        }
    }
    if let Some(disposition) = disposition {
        let mut lines = disposition
            .reason
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        if let Some(first) = lines.next() {
            write!(out, "{SEPARATOR}{first}")?;
        }
        for line in lines {
            write!(out, " {line}")?;
        }
    }
    Ok(())
}
