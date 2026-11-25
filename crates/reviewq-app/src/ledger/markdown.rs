//! Markdown rules for authored ledger fields, judged by the parser the reader uses.
//!
//! A field is checked when it is filed and stored as written. The composer later writes each
//! field as its own block and shifts its headings, so a field that passes here cannot open or
//! swallow a section or card of the report.

use std::borrow::Cow;
use std::fmt::{self, Display, Formatter};
use std::ops::Range;

use satteri_pulldown_cmark::{Event, Options, Parser, Tag};

use super::NoteSection;

/// Longest line of an authored field, in UTF-8 bytes without its line ending.
pub const MAX_FIELD_LINE_BYTES: usize = 8 * 1024;

/// Paragraphs a `summary` note may hold. A `rationale` holds one.
const MAX_SUMMARY_PARAGRAPHS: usize = 4;

/// Labels the report's cover lines use. The reader recognizes a paragraph that starts with
/// one of them as a cover line.
const HEADER_LABELS: [&str; 4] = [
    "Scope:",
    "Reviewed source:",
    "Reviewed target:",
    "Checked at:",
];

/// Appended after a field to test that a heading written after it still reads as one. Only
/// its byte offset identifies it, so its text cannot collide with a field's own heading.
const SENTINEL: &str = "\n\n# End of field\n";

/// Bytes of `SENTINEL` before its heading starts.
const SENTINEL_GAP: usize = 2;

/// The kind of authored field, which decides the rules beyond the line limit and the
/// stand-alone rule that every field follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownField {
    /// Begins with a paragraph, which follows the card's `Issue` label.
    Issue,
    /// Begins with a paragraph, which follows the card's `Why it matters` label.
    Consequence,
    /// The evidence text, observed or gap. Begins with a paragraph.
    Evidence,
    /// Begins with a paragraph, which follows the card's `Proposed comment` label.
    ProposedComment,
    /// A finding's `details`.
    Details,
    /// At most four plain paragraphs.
    Summary,
    /// One plain paragraph.
    Rationale,
    /// The body of any other Markdown note: `flow`, `changes`, `threads`, `questions`,
    /// `verification`, `publication` or an `extra` note.
    Note,
}

impl MarkdownField {
    /// The rules for a note's body. `None` for the `title` note, which is one line of plain
    /// text that the protocol checks.
    pub fn for_note(section: NoteSection) -> Option<Self> {
        match section {
            NoteSection::Title => None,
            NoteSection::Summary => Some(Self::Summary),
            NoteSection::Rationale => Some(Self::Rationale),
            NoteSection::Flow
            | NoteSection::Changes
            | NoteSection::Threads
            | NoteSection::Questions
            | NoteSection::Verification
            | NoteSection::Publication
            | NoteSection::Extra => Some(Self::Note),
        }
    }
}

/// The first rule a field breaks, at the 1-based line of the field where the problem starts.
/// `Display` writes the message the caller returns as the field's error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldProblem {
    pub line: usize,
    pub kind: ProblemKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProblemKind {
    /// The line exceeds `MAX_FIELD_LINE_BYTES`.
    LongLine,
    /// A code fence opens here and never closes, so it would swallow what follows the field.
    OpenFence,
    /// An HTML block starts here and never ends, such as an unclosed comment or `<pre>`.
    OpenHtml,
    /// Another block starts here and runs past the end of the field.
    OpenBlock,
    /// The field holds no Markdown block, only blank space.
    Empty,
    /// The field must begin with a paragraph but begins with another block here.
    NoOpeningParagraph,
    /// A `summary` or `rationale` holds a block other than a paragraph here.
    NotPlain,
    /// A `summary` or `rationale` holds a paragraph beyond its limit here.
    TooManyParagraphs { max: usize },
    /// A `summary` or `rationale` paragraph starts with bold text here.
    BoldStart,
    /// A `summary` or `rationale` paragraph starts with a label the report header uses.
    LabelStart(&'static str),
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
            ProblemKind::Empty => {
                output.write_str("The text is blank. Write it, or leave the field out.")
            }
            ProblemKind::NoOpeningParagraph => write!(
                output,
                "Line {line}: begin with a sentence, because the first words follow the card's label. Put headings, lists, tables, quotes and code after it."
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
        }
    }
}

/// Checks one non-empty authored field. Every field keeps each line within
/// `MAX_FIELD_LINE_BYTES` and stands alone: a heading written after it still reads as a
/// heading, so an open code fence or an HTML block that never ends is refused. `field` adds
/// its own rules. Returns the first problem found. An empty note body removes the note and
/// is not checked.
pub fn check_field(field: MarkdownField, text: &str) -> Result<(), FieldProblem> {
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
    let mut blocks = top_level_blocks(&source);
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
                BlockKind::Paragraph { .. } | BlockKind::Heading | BlockKind::Other => {
                    ProblemKind::OpenBlock
                }
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
            BlockKind::Paragraph { .. } => Ok(()),
            BlockKind::Heading | BlockKind::Code | BlockKind::Html | BlockKind::Other => {
                Err(FieldProblem {
                    line: line_of(text, first.start),
                    kind: ProblemKind::NoOpeningParagraph,
                })
            }
        },
        MarkdownField::Summary => plain_paragraphs(text, &blocks, MAX_SUMMARY_PARAGRAPHS),
        MarkdownField::Rationale => plain_paragraphs(text, &blocks, 1),
        MarkdownField::Details | MarkdownField::Note => Ok(()),
    }
}

/// Writes every heading of an authored field, ATX or setext, as an ATX heading shifted so the
/// shallowest is level four and deeper levels stop at six. Every other line is returned
/// unchanged, and text without a heading is returned borrowed.
///
/// A heading inside a quote or list item is rewritten in place and keeps its container
/// markers. Setext text over several lines is joined into one line with single spaces. Text
/// that ends in `#` gets a closing ` #`, so the ATX form keeps that character.
pub fn shift_headings(text: &str) -> Cow<'_, str> {
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
            .content
            .iter()
            .flat_map(|piece| text[piece.clone()].split('\n'))
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

/// The parser's offset events for `text`, with the options of the reader's display index
/// (`reports/display/cutter.rs`), so a field is judged as the reader will parse the report.
/// The parser drops a leading byte-order mark and reports offsets without it, so leading marks
/// are skipped here and their length added back to every range.
fn events(text: &str) -> impl Iterator<Item = (Event<'_>, Range<usize>)> {
    let skip = text.len() - text.trim_start_matches('\u{feff}').len();
    Parser::new_ext(
        &text[skip..],
        Options::ENABLE_GFM
            | Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
    )
    .into_offset_iter()
    .map(move |(event, range)| (event, range.start + skip..range.end + skip))
}

#[derive(Clone, Copy)]
enum BlockKind {
    Paragraph { bold_start: bool },
    Heading,
    Code,
    Html,
    Other,
}

struct Block {
    start: usize,
    kind: BlockKind,
}

/// The document's top-level blocks in order, each with its starting byte.
fn top_level_blocks(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut depth = 0_usize;
    let mut paragraph_opened = false;
    for (event, range) in events(source) {
        if std::mem::take(&mut paragraph_opened)
            && matches!(event, Event::Start(Tag::Strong))
            && let Some(Block {
                kind: BlockKind::Paragraph { bold_start },
                ..
            }) = blocks.last_mut()
        {
            *bold_start = true;
        }
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    let kind = match tag {
                        Tag::Paragraph => {
                            paragraph_opened = true;
                            BlockKind::Paragraph { bold_start: false }
                        }
                        Tag::Heading { .. } => BlockKind::Heading,
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
            Event::End(_) => depth = depth.saturating_sub(1),
            // A leaf block such as a thematic break.
            _ if depth == 0 => blocks.push(Block {
                start: range.start,
                kind: BlockKind::Other,
            }),
            _ => {}
        }
    }
    blocks
}

/// Every block is a paragraph that starts with plain words, and there are at most `max`.
fn plain_paragraphs(text: &str, blocks: &[Block], max: usize) -> Result<(), FieldProblem> {
    for (index, block) in blocks.iter().enumerate() {
        let line = line_of(text, block.start);
        let kind = match block.kind {
            BlockKind::Heading | BlockKind::Code | BlockKind::Html | BlockKind::Other => {
                Some(ProblemKind::NotPlain)
            }
            BlockKind::Paragraph { bold_start: true } => Some(ProblemKind::BoldStart),
            BlockKind::Paragraph { bold_start: false } => header_label(&text[block.start..])
                .map(ProblemKind::LabelStart)
                .or((index >= max).then_some(ProblemKind::TooManyParagraphs { max })),
        };
        if let Some(kind) = kind {
            return Err(FieldProblem { line, kind });
        }
    }
    Ok(())
}

/// The header label `paragraph` starts with, ignoring ASCII case.
fn header_label(paragraph: &str) -> Option<&'static str> {
    HEADER_LABELS.into_iter().find(|label| {
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
    /// Its inline text, split at line breaks. Each piece is one contiguous source slice, so
    /// escapes and code spans stay as written.
    content: Vec<Range<usize>>,
}

/// Every heading of `text`, in source order.
fn headings(text: &str) -> Vec<Heading> {
    let mut found = Vec::new();
    let mut current: Option<Heading> = None;
    let mut piece: Option<Range<usize>> = None;
    let mut depth = 0_usize;
    for (event, range) in events(text) {
        let Some(heading) = current.as_mut() else {
            if let Event::Start(Tag::Heading { level, .. }) = event {
                let mut end = range.end;
                while end > range.start && matches!(text.as_bytes()[end - 1], b'\n' | b'\r') {
                    end -= 1;
                }
                current = Some(Heading {
                    level: level as u8,
                    replace: range.start..end,
                    content: Vec::new(),
                });
            }
            continue;
        };
        match event {
            Event::End(_) if depth == 0 => {
                heading.content.extend(piece.take());
                found.extend(current.take());
            }
            Event::End(_) => depth -= 1,
            Event::SoftBreak | Event::HardBreak if depth == 0 => {
                heading.content.extend(piece.take());
            }
            event => {
                if depth == 0 {
                    piece = Some(match piece.take() {
                        Some(open) => open.start..range.end.max(open.end),
                        None => range,
                    });
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
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
        ];
        for (case, field, text, expected) in cases {
            assert_eq!(check_field(field, text), expected, "{case}");
        }
        assert_eq!(
            shift_headings("Intro.\n\nPlan\n====\n\nBody.\n"),
            "Intro.\n\n#### Plan\n\nBody.\n",
            "a setext heading is shifted"
        );
    }
}
