//! Markdown rules for authored ledger fields, judged by the parser the reader uses.
//!
//! A field is checked when it is filed and stored as written. The composer later writes each
//! field as its own block and shifts its headings, so a field that passes here cannot open or
//! swallow a section or card of the report.

use std::borrow::Cow;
use std::fmt::{self, Display, Formatter};
use std::ops::Range;

use satteri_pulldown_cmark::{Event, Parser, Tag, TagEnd};

use super::NoteSection;
use super::layout::{COVER_LABELS, CardLabel, CountsLine};
use crate::reports::REPORT_MARKDOWN;

/// Longest line of an authored field, in UTF-8 bytes without its line ending.
pub(crate) const MAX_FIELD_LINE_BYTES: usize = 8 * 1024;

/// Paragraphs a `summary` note may hold. A `rationale` holds one.
const MAX_SUMMARY_PARAGRAPHS: usize = 4;

/// Appended after a field to test that a heading written after it still reads as one. Only
/// its byte offset identifies it, so its text cannot collide with a field's own heading.
const SENTINEL: &str = "\n\n# End of field\n";

/// Bytes of `SENTINEL` before its heading starts.
const SENTINEL_GAP: usize = 2;

/// The kind of authored field, which decides the rules beyond the line limit and the
/// stand-alone rule that every field follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkdownField {
    /// Begins with a paragraph, which follows the card's `Issue` label.
    Issue,
    /// Begins with a paragraph, which follows the card's `Why it matters` label.
    Consequence,
    /// The evidence text, observed or gap. Begins with a paragraph.
    Evidence,
    /// Begins with a paragraph, which follows the card's `Proposed comment` label.
    ProposedComment,
    /// A finding's `details`, written after the evidence, which may end in a list. It may
    /// begin with any block but a list or indented text, which would join that list.
    Details,
    /// At most four plain paragraphs.
    Summary,
    /// One plain paragraph.
    Rationale,
    /// The `publication` note, written after ReviewQ's list of disposition lines. Like
    /// `details`, it begins with no list or indented text.
    Publication,
    /// The body of any other Markdown note: `flow`, `changes`, `threads`, `questions`,
    /// `verification` or an `extra` note.
    Note,
}

impl MarkdownField {
    /// The rules for a note's body. `None` for the `title` note, which is one line of plain
    /// text that the protocol checks.
    pub(crate) fn for_note(section: NoteSection) -> Option<Self> {
        match section {
            NoteSection::Title => None,
            NoteSection::Summary => Some(Self::Summary),
            NoteSection::Rationale => Some(Self::Rationale),
            NoteSection::Flow
            | NoteSection::Changes
            | NoteSection::Threads
            | NoteSection::Questions
            | NoteSection::Verification
            | NoteSection::Extra => Some(Self::Note),
            NoteSection::Publication => Some(Self::Publication),
        }
    }
}

/// The first rule a field breaks, at the 1-based line of the field where the problem starts.
/// `Display` writes the message the caller returns as the field's error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FieldProblem {
    pub(crate) line: usize,
    pub(crate) kind: ProblemKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProblemKind {
    /// The line exceeds `MAX_FIELD_LINE_BYTES`.
    LongLine,
    /// A code fence opens here and never closes, so it would swallow what follows the field.
    OpenFence,
    /// An HTML block starts here and never ends, such as an unclosed comment or `<pre>`.
    OpenHtml,
    /// Another block starts here and runs past the end of the field.
    OpenBlock,
    /// A link reference definition starts here. Once the report is composed it would apply
    /// to every field, and it makes the reader index the whole document at once.
    ReferenceDefinition,
    /// A footnote definition or reference starts here. Like a link reference definition, it
    /// would resolve across fields once the report is composed.
    Footnote,
    /// The field holds no Markdown block, only blank space.
    Empty,
    /// The field must begin with a paragraph but begins with another block here.
    NoOpeningParagraph,
    /// The field follows a list in the report and begins with a list or indented text here,
    /// which would join that list.
    JoinsList,
    /// A `summary` or `rationale` holds a block other than a paragraph here.
    NotPlain,
    /// A `summary` or `rationale` holds a paragraph beyond its limit here.
    TooManyParagraphs { max: usize },
    /// A `summary` or `rationale` paragraph starts with bold text here.
    BoldStart,
    /// A `summary` or `rationale` paragraph starts with a label the report header uses.
    LabelStart(&'static str),
    /// A `summary` or `rationale` paragraph reads as the report's counts line.
    CountsLine,
    /// A finding field's paragraph starts with one of the card's bold labels here.
    CardLabel(CardLabel),
}

impl Display for FieldProblem {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        let line = self.line;
        match self.kind {
            ProblemKind::LongLine => write!(
                output,
                "Line {line} is longer than 8 KiB. Break it into shorter lines."
            ),
            ProblemKind::OpenFence => write!(
                output,
                "Line {line} opens a code fence that is never closed, so it would swallow the rest of the report. Close the fence."
            ),
            ProblemKind::OpenHtml => write!(
                output,
                "Line {line} starts an HTML block that never ends, such as an unclosed comment or <pre>, so it would swallow the rest of the report. Close it."
            ),
            ProblemKind::OpenBlock => write!(
                output,
                "Line {line} starts a block that never ends, so it would swallow the rest of the report. Close it."
            ),
            ProblemKind::ReferenceDefinition => write!(
                output,
                "Line {line} defines a link reference, which would apply to the whole report. Write the link inline as [text](url)."
            ),
            ProblemKind::Footnote => write!(
                output,
                "Line {line} uses a footnote, which would apply to the whole report. Write the note as a sentence or in parentheses instead."
            ),
            ProblemKind::Empty => {
                output.write_str("The text is blank. Write it, or leave the field out.")
            }
            ProblemKind::NoOpeningParagraph => write!(
                output,
                "Line {line}: begin with a sentence, because the first words follow the card's label. Put headings, lists, tables, quotes and code after it."
            ),
            ProblemKind::JoinsList => write!(
                output,
                "Line {line}: begin with a sentence that is not indented. In the report this text can follow a list, and a list or indented text here would join it. Put lists after the first sentence."
            ),
            ProblemKind::NotPlain => write!(
                output,
                "Line {line}: use plain paragraphs only, with no heading, list, table, quote, code or HTML."
            ),
            ProblemKind::TooManyParagraphs { max: 1 } => {
                write!(output, "Line {line}: use one paragraph.")
            }
            ProblemKind::TooManyParagraphs { max } => {
                write!(output, "Line {line}: use at most {max} paragraphs.")
            }
            ProblemKind::BoldStart => write!(
                output,
                "Line {line}: start the paragraph with plain words, not bold text."
            ),
            ProblemKind::LabelStart(label) => write!(
                output,
                "Line {line}: start the paragraph with plain words, not \"{label}\", which the report header uses."
            ),
            ProblemKind::CountsLine => write!(
                output,
                "Line {line}: this paragraph reads as the report's line of finding counts, which ReviewQ writes itself. Say it in a sentence instead."
            ),
            ProblemKind::CardLabel(label) => write!(
                output,
                "Line {line}: start the paragraph with plain words, not \"**{}**\", which the card shows as a field label.",
                label.text()
            ),
        }
    }
}

/// Checks one non-empty authored field. Every field keeps each line within
/// `MAX_FIELD_LINE_BYTES`, stands alone (a heading written after it still reads as a heading,
/// so an open code fence or an HTML block that never ends is refused), defines no link
/// reference and holds no footnote. `field` adds its own rules: a finding's fields begin no
/// paragraph with a card's bold label, all but `details` begin with a paragraph, and
/// `details` and the `publication` note begin with no list or indented text. Returns the
/// first problem found.
///
/// A removal is never passed here: an empty note body removes the note, and a change that
/// removes `proposedComment` or `details` carries no text.
pub(crate) fn check_field(field: MarkdownField, text: &str) -> Result<(), FieldProblem> {
    if let Some(line) = long_line(text) {
        return Err(FieldProblem {
            line,
            kind: ProblemKind::LongLine,
        });
    }
    let mut source = String::with_capacity(text.len() + SENTINEL.len());
    source.push_str(text);
    source.push_str(SENTINEL);
    let sentinel = text.len() + SENTINEL_GAP;
    let parse = Parse::new(&source);
    let definition = parse.first_definition();
    let (mut blocks, footnote) = top_level_blocks(parse);
    match blocks.pop() {
        Some(Block {
            start,
            kind: BlockKind::Heading,
        }) if start == sentinel => {}
        // The last block started inside the field and ran over the heading after it.
        Some(Block { start, kind }) => {
            let kind = match kind {
                BlockKind::Code => ProblemKind::OpenFence,
                BlockKind::Html => ProblemKind::OpenHtml,
                BlockKind::Paragraph { .. }
                | BlockKind::Heading
                | BlockKind::List
                | BlockKind::Other => ProblemKind::OpenBlock,
            };
            return Err(FieldProblem {
                line: line_of(text, start),
                kind,
            });
        }
        None => {
            return Err(FieldProblem {
                line: 1,
                kind: ProblemKind::OpenBlock,
            });
        }
    }
    if let Some(start) = definition {
        return Err(FieldProblem {
            line: line_of(text, start),
            kind: ProblemKind::ReferenceDefinition,
        });
    }
    if let Some(start) = footnote {
        return Err(FieldProblem {
            line: line_of(text, start),
            kind: ProblemKind::Footnote,
        });
    }
    let Some(first) = blocks.first() else {
        return Err(FieldProblem {
            line: 1,
            kind: ProblemKind::Empty,
        });
    };
    match field {
        MarkdownField::Issue
        | MarkdownField::Consequence
        | MarkdownField::Evidence
        | MarkdownField::ProposedComment => match first.kind {
            BlockKind::Paragraph { .. } => card_labels(text, &blocks),
            BlockKind::Heading
            | BlockKind::List
            | BlockKind::Code
            | BlockKind::Html
            | BlockKind::Other => Err(FieldProblem {
                line: line_of(text, first.start),
                kind: ProblemKind::NoOpeningParagraph,
            }),
        },
        MarkdownField::Details => {
            joins_list(text, first)?;
            card_labels(text, &blocks)
        }
        MarkdownField::Publication => joins_list(text, first),
        MarkdownField::Summary => plain_paragraphs(text, &blocks, MAX_SUMMARY_PARAGRAPHS),
        MarkdownField::Rationale => plain_paragraphs(text, &blocks, 1),
        MarkdownField::Note => Ok(()),
    }
}

/// Writes every heading of an authored field, ATX or setext, as an ATX heading shifted so the
/// shallowest is level four and deeper levels stop at six. Every other line is returned
/// unchanged, and text without a heading is returned borrowed.
///
/// A heading inside a quote or list item is rewritten in place and keeps its container
/// markers. Setext text over several lines is joined into one line with single spaces. Text
/// that ends in `#` gets a closing ` #`, so the ATX form keeps that character.
pub(crate) fn shift_headings(text: &str) -> Cow<'_, str> {
    let headings = headings(text);
    let Some(shallowest) = headings.iter().map(|heading| heading.level).min() else {
        return Cow::Borrowed(text);
    };
    let mut shifted = String::with_capacity(text.len() + 4 * headings.len());
    let mut copied = 0;
    for heading in &headings {
        shifted.push_str(&text[copied..heading.replace.start]);
        let level = (heading.level + 4 - shallowest).min(6);
        shifted.extend(std::iter::repeat_n('#', usize::from(level)));
        let mut wrote = false;
        for line in heading
            .lines
            .iter()
            .flat_map(|line| text[line.clone()].split('\n'))
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            shifted.push(' ');
            shifted.push_str(line);
            wrote = true;
        }
        if wrote && shifted.ends_with('#') {
            shifted.push_str(" #");
        }
        copied = heading.replace.end;
    }
    shifted.push_str(&text[copied..]);
    Cow::Owned(shifted)
}

/// `text` parsed with the reader's options (`REPORT_MARKDOWN`), so a field is judged as the
/// reader will parse the report. The parser drops a leading byte-order mark and reports
/// offsets without it, so leading marks are skipped here and their length added back to every
/// offset.
struct Parse<'a> {
    parser: Parser<'a>,
    skip: usize,
}

impl<'a> Parse<'a> {
    fn new(text: &'a str) -> Self {
        let skip = text.len() - text.trim_start_matches('\u{feff}').len();
        Self {
            parser: Parser::new_ext(&text[skip..], REPORT_MARKDOWN),
            skip,
        }
    }

    /// Where the first link reference definition starts. Definitions emit no event, so the
    /// parser's table of them is read before the events.
    fn first_definition(&self) -> Option<usize> {
        self.parser
            .reference_definitions()
            .iter()
            .map(|(_, definition)| definition.span.start + self.skip)
            .min()
    }

    /// The parser's events, each with its source range.
    fn events(self) -> impl Iterator<Item = (Event<'a>, Range<usize>)> {
        let skip = self.skip;
        self.parser
            .into_offset_iter()
            .map(move |(event, range)| (event, range.start + skip..range.end + skip))
    }
}

enum BlockKind {
    Paragraph {
        /// The text of the strong emphasis the paragraph begins with, if it begins with one.
        bold: Option<String>,
        /// The paragraph's text as the reader shows it: its text, code and raw HTML, with a
        /// line break at each soft or hard break.
        text: String,
    },
    Heading,
    List,
    Code,
    Html,
    Other,
}

struct Block {
    start: usize,
    kind: BlockKind,
}

/// The document's top-level blocks in order, each with its starting byte, and where the first
/// footnote definition or reference starts at any depth. The parser emits a reference only when
/// its definition is in the same text, so a lone `[^1]` reads as text and is not reported.
fn top_level_blocks(parse: Parse<'_>) -> (Vec<Block>, Option<usize>) {
    let mut blocks = Vec::new();
    let mut footnote = None;
    let mut depth = 0_usize;
    let mut paragraph_opened = false;
    // The text of a top-level paragraph's leading strong emphasis while it is read.
    let mut bold: Option<String> = None;
    // Whether a top-level paragraph is being read.
    let mut in_paragraph = false;
    for (event, range) in parse.events() {
        if in_paragraph
            && let Some(Block {
                kind: BlockKind::Paragraph { text, .. },
                ..
            }) = blocks.last_mut()
        {
            match &event {
                Event::Text(value)
                | Event::Code(value)
                | Event::Html(value)
                | Event::InlineHtml(value) => text.push_str(value),
                Event::SoftBreak | Event::HardBreak => text.push('\n'),
                _ => {}
            }
        }
        if footnote.is_none()
            && matches!(
                event,
                Event::Start(Tag::FootnoteDefinition(_)) | Event::FootnoteReference(_)
            )
        {
            footnote = Some(range.start);
        }
        if std::mem::take(&mut paragraph_opened) && matches!(event, Event::Start(Tag::Strong)) {
            bold = Some(String::new());
        }
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    let kind = match tag {
                        Tag::Paragraph => {
                            paragraph_opened = true;
                            in_paragraph = true;
                            BlockKind::Paragraph {
                                bold: None,
                                text: String::new(),
                            }
                        }
                        Tag::Heading { .. } => BlockKind::Heading,
                        Tag::List(..) => BlockKind::List,
                        Tag::CodeBlock(_) => BlockKind::Code,
                        Tag::HtmlBlock => BlockKind::Html,
                        _ => BlockKind::Other,
                    };
                    blocks.push(Block {
                        start: range.start,
                        kind,
                    });
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    in_paragraph = false;
                }
                // The leading strong opened inside the paragraph, at depth 1, and ends here.
                if depth == 1
                    && let Some(text) = bold.take()
                    && let Some(Block {
                        kind: BlockKind::Paragraph { bold, .. },
                        ..
                    }) = blocks.last_mut()
                {
                    *bold = Some(text);
                }
            }
            Event::Text(text) | Event::Code(text) if bold.is_some() => {
                if let Some(bold) = bold.as_mut() {
                    bold.push_str(&text);
                }
            }
            // A leaf block such as a thematic break.
            _ if depth == 0 => blocks.push(Block {
                start: range.start,
                kind: BlockKind::Other,
            }),
            _ => {}
        }
    }
    (blocks, footnote)
}

/// No top-level paragraph begins with a card's bold label, such as `**Issue:**`, which the
/// reader would take for a field of the card. The label must be the whole strong text, as the
/// reader matches it.
fn card_labels(text: &str, blocks: &[Block]) -> Result<(), FieldProblem> {
    for block in blocks {
        if let BlockKind::Paragraph {
            bold: Some(bold), ..
        } = &block.kind
            && let Some(label) = CardLabel::ALL
                .into_iter()
                .find(|label| label.text() == bold)
        {
            return Err(FieldProblem {
                line: line_of(text, block.start),
                kind: ProblemKind::CardLabel(label),
            });
        }
    }
    Ok(())
}

/// Every block is a paragraph that starts with plain words and does not read as the counts
/// line, and there are at most `max`.
fn plain_paragraphs(text: &str, blocks: &[Block], max: usize) -> Result<(), FieldProblem> {
    for (index, block) in blocks.iter().enumerate() {
        let line = line_of(text, block.start);
        let kind = match &block.kind {
            BlockKind::Heading
            | BlockKind::List
            | BlockKind::Code
            | BlockKind::Html
            | BlockKind::Other => Some(ProblemKind::NotPlain),
            BlockKind::Paragraph { bold: Some(_), .. } => Some(ProblemKind::BoldStart),
            BlockKind::Paragraph {
                bold: None,
                text: shown,
            } => header_label(&text[block.start..])
                .map(ProblemKind::LabelStart)
                .or(CountsLine::matches(shown).then_some(ProblemKind::CountsLine))
                .or((index >= max).then_some(ProblemKind::TooManyParagraphs { max })),
        };
        if let Some(kind) = kind {
            return Err(FieldProblem { line, kind });
        }
    }
    Ok(())
}

/// Refuses a field that follows a list in the report when its first block, `first`, is a list
/// or starts on an indented line: either would continue that list.
fn joins_list(text: &str, first: &Block) -> Result<(), FieldProblem> {
    let line_start = text[..first.start].rfind('\n').map_or(0, |index| index + 1);
    let indented = text[line_start..].starts_with([' ', '\t']);
    if matches!(first.kind, BlockKind::List) || indented {
        return Err(FieldProblem {
            line: line_of(text, first.start),
            kind: ProblemKind::JoinsList,
        });
    }
    Ok(())
}

/// The cover label `paragraph` starts with, ignoring ASCII case.
fn header_label(paragraph: &str) -> Option<&'static str> {
    COVER_LABELS.into_iter().find(|label| {
        paragraph
            .get(..label.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(label))
    })
}

/// The 1-based number of the first line longer than `MAX_FIELD_LINE_BYTES`.
fn long_line(text: &str) -> Option<usize> {
    text.split('\n')
        .position(|line| line.strip_suffix('\r').unwrap_or(line).len() > MAX_FIELD_LINE_BYTES)
        .map(|index| index + 1)
}

/// The 1-based line of `text` that holds byte `offset`.
fn line_of(text: &str, offset: usize) -> usize {
    let end = offset.min(text.len());
    text.as_bytes()[..end]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

struct Heading {
    level: u8,
    /// The heading's source without its final line ending.
    replace: Range<usize>,
    /// Its inline text, one source slice per line, so escapes and code spans stay as written.
    /// A line starts at its first inline, after any quote marker or indentation the
    /// containers put before it.
    lines: Vec<Range<usize>>,
}

/// Every heading of `text`, in source order.
fn headings(text: &str) -> Vec<Heading> {
    let mut found = Vec::new();
    let mut current: Option<Heading> = None;
    let mut line: Option<Range<usize>> = None;
    // Where the current line's text may start: past the last line break.
    let mut floor = 0;
    for (event, range) in Parse::new(text).events() {
        let Some(heading) = current.as_mut() else {
            if let Event::Start(Tag::Heading { level, .. }) = event {
                let mut end = range.end;
                while end > range.start && matches!(text.as_bytes()[end - 1], b'\n' | b'\r') {
                    end -= 1;
                }
                floor = range.start;
                current = Some(Heading {
                    level: level as u8,
                    replace: range.start..end,
                    lines: Vec::new(),
                });
            }
            continue;
        };
        match event {
            Event::End(TagEnd::Heading(_)) => {
                heading.lines.extend(line.take());
                found.extend(current.take());
            }
            // A break at any depth ends the line, even inside emphasis or a link that goes
            // on, so the next line's container markers are never part of its text.
            Event::SoftBreak | Event::HardBreak => {
                heading.lines.extend(
                    line.take()
                        .map(|open| open.start..open.end.min(range.start)),
                );
                floor = range.end;
            }
            // An inline that opened on an earlier line reports its whole range, so a line
            // starts no earlier than the break before it.
            _ => {
                line = Some(match line.take() {
                    Some(open) => open.start..open.end.max(range.end),
                    None => range.start.max(floor)..range.end,
                });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stand_alone_fields() {
        let refused = |line, kind| Err(FieldProblem { line, kind });
        let cases = [
            (
                "an open top-level fence",
                MarkdownField::Details,
                "Setup.\n\n```rust\nlet retry = 1;\n",
                refused(3, ProblemKind::OpenFence),
            ),
            (
                "an open fence inside a list item",
                MarkdownField::Details,
                "Steps.\n\n- run\n  ```\n  cargo test\n",
                Ok(()),
            ),
            (
                "an unclosed HTML comment",
                MarkdownField::Evidence,
                "Seen in the log.\n\n<!-- trimmed output\n",
                refused(3, ProblemKind::OpenHtml),
            ),
            (
                "a summary paragraph that starts with Scope:",
                MarkdownField::Summary,
                "Adds retries to invoice writes.\n\nScope: the invoice path only.",
                refused(3, ProblemKind::LabelStart("Scope:")),
            ),
            (
                "a details paragraph that starts with a card label",
                MarkdownField::Details,
                "Context.\n\n**Proposed comment:** Ship it.",
                refused(3, ProblemKind::CardLabel(CardLabel::ProposedComment)),
            ),
            (
                "a footnote definition",
                MarkdownField::Note,
                "Retries are unbounded.\n\n[^1]: Seen in the worker log.\n",
                refused(3, ProblemKind::Footnote),
            ),
            (
                "a publication note that begins with a list",
                MarkdownField::Publication,
                "- Posted F-01 on the merge request.\n",
                refused(1, ProblemKind::JoinsList),
            ),
            (
                "details that begin with a sentence and then a list",
                MarkdownField::Details,
                "The replay steps:\n\n- drop the reply\n- retry\n",
                Ok(()),
            ),
            (
                "a summary paragraph that reads as the counts line",
                MarkdownField::Summary,
                "Adds retries.\n\nP0: 1 · P1: 0 · P2: 0 · P3: 0 · Blocking: 1 · Nonblocking: *0*",
                refused(3, ProblemKind::CountsLine),
            ),
            (
                "a rationale that states the counts within a sentence",
                MarkdownField::Rationale,
                "One finding blocks: P0: 1 · P1: 0 · P2: 0 · P3: 0 · Blocking: 1 · Nonblocking: 0",
                Ok(()),
            ),
        ];
        for (case, field, text, expected) in cases {
            assert_eq!(check_field(field, text), expected, "{case}");
        }
        let shifted = [
            (
                "a setext heading is shifted",
                "Intro.\n\nPlan\n====\n\nBody.\n",
                "Intro.\n\n#### Plan\n\nBody.\n",
            ),
            (
                "a quoted setext heading over two lines keeps one quote marker",
                "> *foo\n> bar*\n> ===",
                "> #### *foo bar*",
            ),
        ];
        for (case, text, expected) in shifted {
            assert_eq!(shift_headings(text), expected, "{case}");
        }
    }
}
