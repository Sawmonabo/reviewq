//! Composes a version's text from a snapshot's cover facts, findings and notes and the
//! version's own facts, in one pass that records each card's and section's span as it
//! writes. Fields were checked when filed, so composing cannot fail. The only parsing is
//! `markdown::shift_headings`, which parses each Markdown field to find its headings, and
//! `markdown::fenced_code`, which finds the fenced code a proposed comment's quote keeps as
//! written. A span runs from its heading to the end of its last line, without the line
//! ending; a section's span holds its cards.

mod output;
#[cfg(test)]
mod tests;

use std::fmt::{self, Write};

use reviewq_domain::identity::GitSha;
use reviewq_domain::ledger::{VersionState, version_state};

use url::Url;

use super::layout::{
    COVER_LABELS, CardLabel, CountsLine, DISCUSSIONS_LABEL, FixedSection, GAP_OPENING,
    NEW_COMMENT_AT, NEW_COMMENT_ON_MR, NO_THREAD_OR_EVIDENCE, PROPOSED_COMMENT_OPENING,
    PUBLICATION_LABEL, REPLY_IN, REPLY_THREAD, SEPARATOR, SHORT_SHA_CHARS, THREAD_LINK,
    THREADS_HEADER, UNKNOWN_REVISION, blocking_word, outcome_label, scope, verdict_line,
};
use super::markdown::shift_headings;
use super::{
    Composed, CoverFacts, FindingOutcome, NoteRecord, NoteSection, SnapshotFinding, VersionFacts,
    default_category,
};
use crate::reports::{
    FindingBasisKind, FindingLocation, FindingSection, RevisionEvidence, Severity,
};
use output::{
    Cell, Output, UtcSeconds, without_blank_lines, write_code, write_heading_text, write_link,
    write_literal,
};

/// Bytes reserved per card or note beyond its record's own text: labels, the card line and
/// the blank lines between blocks.
const BLOCK_OVERHEAD_BYTES: usize = 256;
/// Bytes reserved for the identity line, the verdict, counts and cover lines, the fixed
/// headings and the lines of empty sections.
const FRAME_BYTES: usize = 2048;
/// Bytes reserved per link beyond the URL it starts from and the text it encodes: its marks,
/// the `/-/blob/{sha}/`, `/-/commit/{sha}` or `#note_{id}` part and its text.
const LINK_BYTES: usize = 160;

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
    /// An upper estimate of the text's size, so the output is allocated once. A location's
    /// link holds its path up to three times percent-encoded and once more as text, and a
    /// quote adds two bytes to every line.
    fn reserve(&self) -> usize {
        let cover = self.cover;
        let link = cover.links.project().as_str().len() + LINK_BYTES;
        let findings: usize = self
            .findings
            .iter()
            .map(|finding| {
                let fields = &finding.record.fields;
                let locations: usize = fields
                    .body
                    .locations
                    .iter()
                    .map(|location| 4 * location.path.len() + link)
                    .sum();
                let comment = fields
                    .body
                    .proposed_comment
                    .as_ref()
                    .map_or(0, |comment| 2 * comment.lines().count() + link);
                let outcome = finding.record.outcome.as_ref().map_or(0, |outcome| {
                    let discussions: usize = outcome
                        .discussion_ids
                        .iter()
                        .map(|id| id.len() + link)
                        .sum();
                    discussions + link
                });
                fields.text_bytes() + BLOCK_OVERHEAD_BYTES + locations + comment + outcome
            })
            .sum();
        let notes: usize = self
            .notes
            .iter()
            .map(|note| {
                note.body.len() + note.key.title().map_or(0, str::len) + BLOCK_OVERHEAD_BYTES
            })
            .sum();
        let tickets: usize = cover
            .tickets
            .iter()
            .map(|key| key.as_str().len() + cover.links.jira_len() + LINK_BYTES)
            .sum();
        FRAME_BYTES
            + cover.project_path.len()
            + 2 * link
            + tickets
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
        self.identity(out)?;
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

    /// `[{project}]({project URL}) · [MR !{iid}]({MR URL})`, then one part per linked ticket:
    /// a link to it on the Jira site, or its key as text without one.
    fn identity(&self, out: &mut Output) -> fmt::Result {
        let cover = self.cover;
        let links = &cover.links;
        out.block()?;
        write_link(out, file_name(&cover.project_path), links.project())?;
        out.write_str(SEPARATOR)?;
        write_link(
            out,
            format_args!("MR !{}", cover.iid),
            links.merge_request(),
        )?;
        for key in &cover.tickets {
            out.write_str(SEPARATOR)?;
            match links.ticket(key) {
                Some(url) => write_link(out, key, &url)?,
                None => write_literal(out, key.as_str())?,
            }
        }
        Ok(())
    }

    /// The link to the GitLab note `discussion` resolves to, if ReviewQ tracks one.
    fn thread(&self, discussion: &str) -> Option<Url> {
        self.cover
            .thread_notes
            .get(discussion)
            .filter(|note| !note.is_empty() && note.bytes().all(|byte| byte.is_ascii_digit()))
            .map(|note| self.cover.links.note(note))
    }

    fn counts(&self, out: &mut Output) -> fmt::Result {
        let mut line = CountsLine {
            severities: [0; 4],
            blocking: 0,
            nonblocking: 0,
        };
        for finding in self.active() {
            let index = match finding.record.fields.severity {
                Severity::P0 => 0,
                Severity::P1 => 1,
                Severity::P2 => 2,
                Severity::P3 => 3,
            };
            line.severities[index] += 1;
            if effective_blocking(finding) {
                line.blocking += 1;
            } else {
                line.nonblocking += 1;
            }
        }
        out.block()?;
        write!(out, "{line}")
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

    /// `Our previous threads`: the `threads` note, then one table row per outcome this
    /// version records.
    fn threads(&self, out: &mut Output) -> fmt::Result {
        let start = out.open_section(FixedSection::Threads)?;
        if let Some(note) = self.note(NoteSection::Threads) {
            out.block()?;
            out.authored(&shift_headings(&note.body))?;
        }
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
        out.close_fixed_section(FixedSection::Threads, start)
    }

    /// One previous-threads row. Its last cell holds, joined by `SEPARATOR`, each linked
    /// discussion in the outcome's order (a `Thread` link to its note, numbered when the row
    /// has several, or its ID as code when ReviewQ tracks no note for it), then the checked
    /// revision as a link to the commit. With neither it holds `NO_THREAD_OR_EVIDENCE`.
    fn threads_row(
        &self,
        out: &mut Output,
        finding: &SnapshotFinding,
        outcome: &FindingOutcome,
    ) -> fmt::Result {
        write!(out, "\n| {}{SEPARATOR}", finding.record.id)?;
        Cell::new(out).write_str(&finding.record.fields.title)?;
        write!(out, " | **{}.** ", outcome_label(outcome.conclusion))?;
        Cell::new(out).write_str(&outcome.reason)?;
        out.write_str(" | ")?;
        let threads: Vec<Option<Url>> = outcome
            .discussion_ids
            .iter()
            .map(|discussion| self.thread(discussion))
            .collect();
        let numbered = threads.iter().flatten().count() > 1;
        let mut cell = Cell::new(out);
        let mut parts = 0;
        let mut number = 0;
        for (discussion, thread) in outcome.discussion_ids.iter().zip(&threads) {
            if parts > 0 {
                cell.write_str(SEPARATOR)?;
            }
            match thread {
                Some(url) => {
                    number += 1;
                    if numbered {
                        write_link(&mut cell, format_args!("{THREAD_LINK} {number}"), url)?;
                    } else {
                        write_link(&mut cell, THREAD_LINK, url)?;
                    }
                }
                None => write_code(&mut cell, discussion, None)?,
            }
            parts += 1;
        }
        if let Some(sha) = checked_revision(outcome, &self.facts.reviewed) {
            if parts > 0 {
                cell.write_str(SEPARATOR)?;
            }
            let short = &sha.as_str()[..SHORT_SHA_CHARS];
            write_link(&mut cell, short, &self.cover.links.commit(sha))?;
            parts += 1;
        }
        if parts == 0 {
            cell.write_str(NO_THREAD_OR_EVIDENCE)?;
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
            self.card(out, finding)?;
        }
        out.close_fixed_section(section, start)
    }

    /// One card: the heading, the card line, then each given field in the reader's order.
    fn card(&self, out: &mut Output, finding: &SnapshotFinding) -> fmt::Result {
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
                self.location(out, location)?;
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
            self.proposed_comment(out, finding, comment)?;
        }
        out.close_card(record.id, start);
        Ok(())
    }

    /// A location as a link to its file at the version's reviewed source, named by the file
    /// and line. With the source unknown, its full path and line as code.
    fn location(&self, out: &mut Output, location: &FindingLocation) -> fmt::Result {
        let RevisionEvidence::Exact(sha) = &self.facts.reviewed else {
            return write_code(out, &location.path, location.line);
        };
        let url = self.cover.links.blob(sha, &location.path, location.line);
        let name = file_name(&location.path);
        match location.line {
            Some(line) => write_link(out, format_args!("{name}:{line}"), &url),
            None => write_link(out, name, &url),
        }
    }

    /// The bold lead line, which says where the comment would go, then the comment as a block
    /// quote. It goes as a reply in the first linked discussion ReviewQ tracks a note for,
    /// otherwise as a new comment at the first location, otherwise on the MR.
    fn proposed_comment(
        &self,
        out: &mut Output,
        finding: &SnapshotFinding,
        comment: &str,
    ) -> fmt::Result {
        out.block()?;
        write!(out, "**{PROPOSED_COMMENT_OPENING}")?;
        let thread = finding
            .discussion_ids
            .iter()
            .find_map(|discussion| self.thread(discussion));
        match (thread, finding.record.fields.body.locations.first()) {
            (Some(url), _) => {
                out.write_str(REPLY_IN)?;
                write_link(out, REPLY_THREAD, &url)?;
            }
            (None, Some(location)) => {
                out.write_str(NEW_COMMENT_AT)?;
                write_literal(out, file_name(&location.path))?;
                if let Some(line) = location.line {
                    write!(out, ":{line}")?;
                }
            }
            (None, None) => out.write_str(NEW_COMMENT_ON_MR)?,
        }
        out.write_str(":**")?;
        out.block()?;
        out.quoted(&shift_headings(comment))
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

/// The last segment of a path that is not empty, or the whole path without one: a
/// location's file name, or a project's name.
fn file_name(path: &str) -> &str {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(path)
}

/// A field whose first paragraph follows its bold label on the same line.
fn labelled_field(out: &mut Output, label: CardLabel, text: &str) -> fmt::Result {
    out.block()?;
    write!(out, "**{}** ", label.text())?;
    out.authored(&shift_headings(text))
}

/// Gap evidence under `Evidence`, beginning `Not confirmed.` unless the agent already began
/// with that phrase in any ASCII case, followed by `.` or `:`.
fn gap_field(out: &mut Output, text: &str) -> fmt::Result {
    out.block()?;
    write!(out, "**{}** ", CardLabel::Evidence.text())?;
    let shifted = shift_headings(text);
    let text = without_blank_lines(&shifted);
    let phrase = GAP_OPENING.trim_end_matches('.');
    let opens = text
        .get(..phrase.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(phrase))
        && matches!(text.as_bytes().get(phrase.len()), Some(b'.' | b':'));
    if !opens {
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
