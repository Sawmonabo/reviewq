//! The composed text as it is written: the output buffer that counts scalars and lines and
//! records spans as pieces are appended, and the Markdown primitives that write a value so it
//! reads as given.

use std::fmt::{self, Display, Formatter, Write};

use reviewq_domain::identity::ReportHash;
use reviewq_domain::ledger::FindingId;
use sha2::{Digest, Sha256};
use url::Url;

use crate::ledger::layout::FixedSection;
use crate::ledger::markdown::fenced_code;
use crate::ledger::{CardSpan, Composed, SectionSpan, Span};
use crate::reports::{ArtifactStats, LineStats};

/// Characters that open or close inline Markdown, escaped where text must read as written:
/// an extra note's title and a card's category. `#` is among them so a title that ends in
/// `#` keeps it instead of losing it as a heading's closing sequence.
const INLINE_MARKUP: [char; 10] = ['\\', '`', '*', '_', '[', ']', '<', '&', '~', '#'];

/// Characters a URL from the `url` crate may keep that a Markdown link destination would end
/// on or decode: parentheses, a backslash escape and a character reference.
const LINK_TARGET_RESERVED: [char; 4] = ['(', ')', '\\', '&'];

/// Columns between Markdown's tab stops.
const TAB_STOP: usize = 4;

/// The composed text as it grows, with its scalar count and line statistics counted from
/// each piece as it is appended, and the spans recorded so far.
pub(super) struct Output {
    text: String,
    scalars: u64,
    lines: LineStats,
    cards: Vec<CardSpan>,
    sections: Vec<SectionSpan>,
}

/// A position in the output, in bytes and scalars.
#[derive(Clone, Copy)]
pub(super) struct Mark {
    byte: usize,
    scalar: u64,
}

impl Output {
    pub(super) fn with_capacity(bytes: usize) -> Self {
        Self {
            text: String::with_capacity(bytes),
            scalars: 0,
            lines: LineStats::default(),
            cards: Vec::new(),
            sections: Vec::with_capacity(FixedSection::ALL.len()),
        }
    }

    /// Starts a block: one blank line after the previous block, nothing at the start.
    pub(super) fn block(&mut self) -> fmt::Result {
        if self.text.is_empty() {
            Ok(())
        } else {
            self.write_str("\n\n")
        }
    }

    /// Authored Markdown without its outer blank lines. Its own lines are written unchanged.
    pub(super) fn authored(&mut self, text: &str) -> fmt::Result {
        self.write_str(without_blank_lines(text))
    }

    /// Authored Markdown without its outer blank lines, as one block quote: each line after
    /// `> `, and an empty line as `>` alone. The marker moves the tab stops, so outside a
    /// top-level fenced code block each tab in a line's indentation and container markers is
    /// written as the spaces it stood for. Every block inside the quote then reads as it did
    /// on its own, and a fenced block keeps its content as written.
    pub(super) fn quoted(&mut self, text: &str) -> fmt::Result {
        let text = without_blank_lines(text);
        let fences = fenced_code(text);
        let mut offset = 0;
        for (index, line) in text.split('\n').enumerate() {
            let start = offset;
            offset += line.len() + 1;
            if index > 0 {
                self.write_char('\n')?;
            }
            if line.trim_end_matches('\r').is_empty() {
                self.write_char('>')?;
                self.write_str(line)?;
                continue;
            }
            self.write_str("> ")?;
            let verbatim = fences
                .iter()
                .any(|fence| fence.start < start && start < fence.end);
            let structure = if verbatim {
                0
            } else {
                line.find(|character: char| !is_structure(character))
                    .unwrap_or(line.len())
            };
            let mut column = 0;
            for character in line[..structure].chars() {
                if character == '\t' {
                    let width = TAB_STOP - column % TAB_STOP;
                    self.write_str(&"    "[..width])?;
                    column += width;
                } else {
                    self.write_char(character)?;
                    column += 1;
                }
            }
            self.write_str(&line[structure..])?;
        }
        Ok(())
    }

    pub(super) fn mark(&self) -> Mark {
        Mark {
            byte: self.text.len(),
            scalar: self.scalars,
        }
    }

    fn span_from(&self, start: Mark) -> Span {
        Span {
            bytes: start.byte..self.text.len(),
            scalar_offset: start.scalar,
            scalar_length: self.scalars - start.scalar,
        }
    }

    /// Records the card written since `start`.
    pub(super) fn close_card(&mut self, id: FindingId, start: Mark) {
        let span = self.span_from(start);
        self.cards.push(CardSpan { id, span });
    }

    /// Writes a fixed section's heading and returns where the section starts.
    pub(super) fn open_section(&mut self, section: FixedSection) -> Result<Mark, fmt::Error> {
        self.block()?;
        let start = self.mark();
        write!(self, "## {}", section.heading())?;
        Ok(start)
    }

    /// Ends a fixed section, writing its empty line when nothing followed the heading.
    pub(super) fn close_fixed_section(
        &mut self,
        section: FixedSection,
        start: Mark,
    ) -> fmt::Result {
        let heading = "## ".len() + section.heading().len();
        if self.text.len() == start.byte + heading {
            self.block()?;
            self.write_str(section.empty_line())?;
        }
        self.close_section(section.heading().to_owned(), start);
        Ok(())
    }

    /// Records the section written since `start` under `title`, its heading as it reads.
    pub(super) fn close_section(&mut self, title: String, start: Mark) {
        let span = self.span_from(start);
        self.sections.push(SectionSpan { title, span });
    }

    /// The text with its digest, statistics and spans.
    pub(super) fn finish(self) -> Composed {
        let sha256 = ReportHash::from_sha256(Sha256::digest(self.text.as_bytes()).into());
        let (line_count, max_line_bytes) = self.lines.finish();
        Composed {
            stats: ArtifactStats {
                byte_length: self.text.len() as u64,
                total_code_points: self.scalars,
                line_count,
                max_line_bytes,
            },
            sha256,
            text: self.text,
            cards: self.cards,
            sections: self.sections,
        }
    }
}

impl Write for Output {
    fn write_str(&mut self, piece: &str) -> fmt::Result {
        self.text.push_str(piece);
        self.scalars += piece.chars().count() as u64;
        self.lines.push(piece.as_bytes());
        Ok(())
    }
}

/// Writes into a table cell so no `|` ends it. A `|` after an odd run of backslashes is
/// already escaped and stays as written; any other gets a backslash. The run is counted
/// across writes, because a formatted write arrives in pieces.
pub(super) struct Cell<'a> {
    out: &'a mut Output,
    /// Backslashes written since the last other character.
    backslashes: usize,
}

impl<'a> Cell<'a> {
    pub(super) fn new(out: &'a mut Output) -> Self {
        Self {
            out,
            backslashes: 0,
        }
    }
}

impl Write for Cell<'_> {
    fn write_str(&mut self, piece: &str) -> fmt::Result {
        let mut copied = 0;
        for (index, byte) in piece.bytes().enumerate() {
            match byte {
                b'\\' => self.backslashes += 1,
                b'|' if self.backslashes.is_multiple_of(2) => {
                    self.out.write_str(&piece[copied..index])?;
                    self.out.write_char('\\')?;
                    copied = index;
                    self.backslashes = 0;
                }
                _ => self.backslashes = 0,
            }
        }
        self.out.write_str(&piece[copied..])
    }
}

/// `text` (and `:line` when given) as one code span whose fence is longer than any backtick
/// run in `text`. A space pads each side when the content starts or ends with a backtick or a
/// space, which the parser strips again, so the content reads exactly as given.
pub(super) fn write_code(out: &mut impl Write, text: &str, line: Option<u32>) -> fmt::Result {
    let fence = text
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
        + 1;
    let edge = |character: Option<char>| matches!(character, Some('`' | ' '));
    let padded = match line {
        Some(_) => edge(text.chars().next()),
        None => edge(text.chars().next()) || edge(text.chars().next_back()),
    };
    let pad = if padded { " " } else { "" };
    for _ in 0..fence {
        out.write_char('`')?;
    }
    out.write_str(pad)?;
    out.write_str(text)?;
    if let Some(line) = line {
        write!(out, ":{line}")?;
    }
    out.write_str(pad)?;
    for _ in 0..fence {
        out.write_char('`')?;
    }
    Ok(())
}

/// A heading's authored text as written. Text that ends in a run of `#` after a space would
/// lose that run as the heading's closing sequence, so it gets a closing ` #` of its own.
pub(super) fn write_heading_text(out: &mut impl Write, text: &str) -> fmt::Result {
    out.write_str(text)?;
    let before = text.trim_end_matches('#');
    if before.len() < text.len() && (before.is_empty() || before.ends_with([' ', '\t'])) {
        out.write_str(" #")?;
    }
    Ok(())
}

/// `text` with every `INLINE_MARKUP` character backslash-escaped, so it reads as written.
pub(super) fn write_literal(out: &mut impl Write, text: &str) -> fmt::Result {
    let mut rest = text;
    while let Some(index) = rest.find(INLINE_MARKUP) {
        out.write_str(&rest[..index])?;
        out.write_char('\\')?;
        out.write_str(&rest[index..=index])?;
        rest = &rest[index + 1..];
    }
    out.write_str(rest)
}

/// Writes each piece through `write_literal`, so formatted text reads as written.
struct Literal<'a, W>(&'a mut W);

impl<W: Write> Write for Literal<'_, W> {
    fn write_str(&mut self, piece: &str) -> fmt::Result {
        write_literal(self.0, piece)
    }
}

/// `[text](url)`: the text as written (`write_literal`), and the URL with each
/// `LINK_TARGET_RESERVED` character percent-encoded, so neither can end the link early or
/// open markup.
pub(super) fn write_link(out: &mut impl Write, text: impl Display, url: &Url) -> fmt::Result {
    out.write_char('[')?;
    write!(Literal(out), "{text}")?;
    out.write_str("](")?;
    let mut rest = url.as_str();
    while let Some(index) = rest.find(LINK_TARGET_RESERVED) {
        out.write_str(&rest[..index])?;
        write!(out, "%{:02X}", rest.as_bytes()[index])?;
        rest = &rest[index + 1..];
    }
    out.write_str(rest)?;
    out.write_char(')')
}

/// Whether `character` can make up a line's indentation and container markers: a space, a
/// tab, a quote marker or part of a list marker.
fn is_structure(character: char) -> bool {
    matches!(
        character,
        ' ' | '\t' | '>' | '-' | '+' | '*' | '.' | ')' | '0'..='9'
    )
}

/// `text` without its leading and trailing blank lines. The remaining lines keep their
/// indentation and trailing spaces.
pub(super) fn without_blank_lines(text: &str) -> &str {
    let Some(first) = text.find(|character: char| !character.is_ascii_whitespace()) else {
        return "";
    };
    let start = text[..first].rfind('\n').map_or(0, |index| index + 1);
    let last = text.trim_ascii_end().len();
    let end = text[last..]
        .find('\n')
        .map_or(text.len(), |index| last + index);
    text[start..end].trim_end_matches('\r')
}

/// Unix milliseconds as an RFC 3339 UTC time to the second, such as `2026-11-25T14:03:07Z`.
pub(super) struct UtcSeconds(pub(super) i64);

impl Display for UtcSeconds {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        const DAY_SECONDS: i64 = 86_400;
        let seconds = self.0.div_euclid(1000);
        let (year, month, day) = civil_date(seconds.div_euclid(DAY_SECONDS));
        let time = seconds.rem_euclid(DAY_SECONDS);
        write!(
            output,
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            time / 3600,
            time % 3600 / 60,
            time % 60
        )
    }
}

/// The proleptic Gregorian date `days` after 1970-01-01, by Howard Hinnant's
/// `civil_from_days`.
fn civil_date(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}
