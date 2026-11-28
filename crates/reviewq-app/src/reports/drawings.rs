use reviewq_domain::identity::ReportHash;
use satteri_pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};

use super::REPORT_MARKDOWN;
use crate::access::{AppError, AppFuture};

const SOURCE_BYTES: usize = 64 * 1024;
const SOURCE_CHARACTERS: usize = 30_000;
const DISCOVERED: usize = 8;

pub const MAX_DRAWING_BYTES: usize = 64 * 1024;
pub const DRAWING_RECIPE: &str = "mermaid-12.2.0-classic-dark-1";

pub struct DrawingOutput {
    pub svg: String,
    /// Fingerprint of the fonts the fixed producer page actually used.
    pub font: ReportHash,
}

pub trait DrawingRenderer: Send + Sync {
    fn can_draw(&self, font: Option<&ReportHash>) -> bool;
    fn draw(&self, source: String) -> AppFuture<'_, DrawingOutput>;
    fn ready_font(&self, source: &ReportHash) -> Result<Option<ReportHash>, AppError>;
    fn publish(&self, source: ReportHash, drawing: DrawingOutput) -> AppFuture<'_, ()>;
    fn read(&self, source: ReportHash, font: ReportHash) -> AppFuture<'_, String>;
    fn expire(&self) -> AppFuture<'_, ()>;
}

/// Whether a fenced code block's info string makes it a Mermaid diagram, as the page decides:
/// its `language-` class takes the info string up to the first space or tab.
pub(crate) fn is_mermaid(kind: &CodeBlockKind<'_>) -> bool {
    matches!(kind, CodeBlockKind::Fenced(info) if info.split([' ', '\t']).next() == Some("mermaid"))
}

/// Read only actual Mermaid fences from authorized Markdown. The consumer also
/// hashes the canonical browser fence before attaching a derivative. Each source is written as
/// `diagram_source` writes it, and only a source the page draws (`undrawable` gives `None`) is
/// returned, so nothing the page shows as plain source is drawn ahead.
pub fn mermaid_sources(markdown: &str) -> Vec<String> {
    if !markdown.contains("mermaid") || markdown.len() > 1024 * 1024 {
        return Vec::new();
    }
    let mut drawings = Vec::new();
    let mut source = None;
    let parser = Parser::new_ext(markdown, REPORT_MARKDOWN);
    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(kind)) if is_mermaid(&kind) => {
                source = Some(String::new());
            }
            Event::Text(text) => {
                if let Some(current) = &mut source {
                    if current.len() + text.len() > SOURCE_BYTES {
                        source = None;
                    } else {
                        current.push_str(&text);
                    }
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(source) = source.take().filter(|source| !source.is_empty()) {
                    let source = diagram_source(&source);
                    if undrawable(&source).is_none() {
                        drawings.push(source);
                        if drawings.len() == DISCOVERED {
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    drawings
}

/// A Mermaid block's text as the page hands it to Mermaid, byte for byte: the block's text
/// with its last line ending, `\r\n`, `\n` or `\r`, written as `\n`, or `\n` added when it
/// has none. The page's syntax tree drops the last line ending and the HTML step adds `\n`,
/// while every other line ending stays as written, `\r` included. So the page's tests count
/// these characters, and the page looks up a drawing made ahead by the SHA-256 of these
/// bytes. Mermaid reads `\r\n` and `\r` as `\n`, so a drawing does not depend on them.
pub(crate) fn diagram_source(text: &str) -> String {
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix(['\n', '\r']))
        .unwrap_or(text);
    format!("{body}\n")
}

/// The key a drawing made ahead is stored and looked up by: the SHA-256 of a diagram's source
/// (`diagram_source`) as UTF-8, written in lowercase hex. The page's `digestDrawing` computes
/// the same key from the source it hands to Mermaid.
pub fn drawing_digest(source: &str) -> ReportHash {
    ReportHash::from_sha256(Sha256::digest(source.as_bytes()).into())
}

/// Why the page shows a Mermaid source as text instead of drawing it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Undrawable {
    /// Longer than `SOURCE_CHARACTERS` UTF-16 code units.
    TooLong,
    /// More than `MAX_ARROWS` arrows.
    TooManyArrows,
    /// Starts, after white space, with a `%%{` directive or `---` front matter.
    Header,
    /// Holds a `click` action, an `http:` or `https:` link, `@import` or `url(`.
    Link,
}

/// Arrows a drawable source may hold.
const MAX_ARROWS: usize = 500;

/// Why the page would not draw `source`, written as `diagram_source` writes it, or `None` when
/// it would. Mirrors `eligibleDiagram` in `web/src/components/markdown/mermaid.ts`, test by test
/// and in its order; change both together. Like that function it counts UTF-16 code units,
/// treats white space as JavaScript's `\s` does and ignores ASCII case only.
pub(crate) fn undrawable(source: &str) -> Option<Undrawable> {
    let units: Vec<u16> = source.encode_utf16().collect();
    if units.len() > SOURCE_CHARACTERS {
        return Some(Undrawable::TooLong);
    }
    if arrows(&units) > MAX_ARROWS {
        return Some(Undrawable::TooManyArrows);
    }
    let start = source.trim_start_matches(js_space);
    if start.starts_with("%%{") || start.starts_with("---") {
        return Some(Undrawable::Header);
    }
    let lower = source.to_ascii_lowercase();
    let click = lower.match_indices("click").any(|(index, word)| {
        !lower[..index]
            .chars()
            .next_back()
            .is_some_and(|before| before.is_ascii_alphanumeric() || before == '_')
            && lower[index + word.len()..]
                .chars()
                .next()
                .is_some_and(js_space)
    });
    let url = lower.match_indices("url").any(|(index, word)| {
        lower[index + word.len()..]
            .trim_start_matches(js_space)
            .starts_with('(')
    });
    (click
        || url
        || ["http:", "https:", "@import"]
            .iter()
            .any(|item| lower.contains(item)))
    .then_some(Undrawable::Link)
}

/// Matches of `/-->|---|==>|-.->|->>/g` in `units`: at each position the first alternative that
/// matches, then the search resumes after it. `.` is one code unit other than a line terminator.
fn arrows(units: &[u16]) -> usize {
    let unit = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
    let [long, rule, thick, open] = [unit("-->"), unit("---"), unit("==>"), unit("->>")];
    let dash = u16::from(b'-');
    let any = |value: u16| !matches!(value, 0x0a | 0x0d | 0x2028 | 0x2029);
    let (mut count, mut index) = (0, 0);
    while index < units.len() {
        let rest = &units[index..];
        let length =
            if rest.starts_with(&long) || rest.starts_with(&rule) || rest.starts_with(&thick) {
                3
            } else if rest.len() >= 4 && rest[0] == dash && any(rest[1]) && rest[2..4] == long[1..]
            {
                4
            } else if rest.starts_with(&open) {
                3
            } else {
                0
            };
        if length == 0 {
            index += 1;
        } else {
            count += 1;
            index += length;
        }
    }
    count
}

/// Whether `character` is white space to JavaScript's `\s`, which is also what JavaScript's
/// `trim()` removes.
pub(crate) fn js_space(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
