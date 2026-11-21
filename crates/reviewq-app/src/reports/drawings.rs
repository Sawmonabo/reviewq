use reviewq_domain::identity::ReportHash;
use satteri_pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

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
    fn draw(&self, source: String) -> AppFuture<'_, DrawingOutput>;
    fn ready_font(&self, source: &ReportHash) -> Result<Option<ReportHash>, AppError>;
    fn publish(&self, source: ReportHash, drawing: DrawingOutput) -> AppFuture<'_, ()>;
    fn read(&self, source: ReportHash, font: ReportHash) -> AppFuture<'_, String>;
    fn expire(&self) -> AppFuture<'_, ()>;
}

/// Read only actual Mermaid fences from authorized Markdown. The consumer also
/// hashes the canonical browser fence before attaching a derivative.
pub fn mermaid_sources(markdown: &str) -> Vec<String> {
    if !markdown.contains("mermaid") || markdown.len() > 1024 * 1024 {
        return Vec::new();
    }
    let mut drawings = Vec::new();
    let mut source = None;
    let parser = Parser::new_ext(
        markdown,
        Options::ENABLE_GFM
            | Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
    );
    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.split_whitespace().next() == Some("mermaid") =>
            {
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
                    let mut source = source.replace("\r\n", "\n");
                    if !source.ends_with('\n') {
                        source.push('\n');
                    }
                    if eligible(&source) {
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

fn eligible(source: &str) -> bool {
    let prefix = source.trim_start();
    if source.encode_utf16().count() > SOURCE_CHARACTERS
        || prefix.starts_with("%%{")
        || prefix.starts_with("---")
    {
        return false;
    }
    let lower = source.to_ascii_lowercase();
    !["http:", "https:", "@import", "click ", "url("]
        .iter()
        .any(|item| lower.contains(item))
}
