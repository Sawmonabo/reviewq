//! Project the same authorized Markdown selection that the browser will open.

use crate::history::document::{Window, WindowResult};
use crate::reading::{FoldedText, Literal, TextMatch, displayed_document, displayed_markdown};

use super::{DocumentMatch, DocumentMatchMode};

pub struct Projection {
    pub proof: DocumentMatch,
    pub text: Option<String>,
    pub previous: Option<u64>,
    pub next: Option<u64>,
    /// Entire body uses the literal reader and can be scanned directly across page boundaries.
    pub literal_body: bool,
    /// Find must supply the source-wide context before matching Markdown.
    pub needs_context: bool,
}

pub fn project(window: Window) -> Projection {
    use DocumentMatchMode::{Canonical, Source, TableBody, Window as Formatted};
    let Window {
        hash,
        bytes,
        previous,
        next,
        result,
        table_start,
        ..
    } = window;
    let mut literal_body = false;
    let mut needs_context = false;
    let (mode, text) = match result {
        WindowResult::Ready(blocks) => {
            let source: String = blocks.into_iter().map(|block| block.markdown).collect();
            (Formatted, displayed_markdown(&source))
        }
        WindowResult::Table(page) if page.header_in_source => {
            (Formatted, displayed_markdown(&page.markdown))
        }
        WindowResult::Table(page) => {
            let mut source = page.header;
            let header = displayed_markdown(&source);
            source.push_str(&page.markdown);
            let text = header.and_then(|header| {
                let projected = displayed_markdown(&source)?;
                let body = projected.strip_prefix(&header)?.strip_prefix('\n')?;
                Some(body.to_owned())
            });
            (TableBody, text)
        }
        WindowResult::Canonical(source) => (Canonical, displayed_document(&source)),
        WindowResult::Source {
            canonical: true, ..
        } => {
            needs_context = true;
            (Canonical, None)
        }
        WindowResult::Source { markdown, .. } => {
            literal_body = table_start.is_none();
            (Source, (!literal_body).then_some(markdown))
        }
    };
    Projection {
        proof: DocumentMatch {
            hash,
            byte_start: bytes.start,
            mode,
        },
        text,
        previous,
        next,
        literal_body,
        needs_context,
    }
}

/// Take the requested end of a bounded display projection before constructing witnesses.
pub fn occurrences(
    text: &str,
    query: &Literal,
    boundary: u64,
    reverse: bool,
    limit: usize,
) -> (Vec<TextMatch>, bool) {
    let folded = FoldedText::new(text);
    let mut matches = folded.occurrences(query);
    let mut selected = Vec::with_capacity(limit);
    let mut partial = false;
    while selected.len() < limit {
        let span = if reverse {
            matches.next_back()
        } else {
            matches.next()
        };
        let Some(span) = span else { break };
        let start = span.start.byte as u64;
        if if reverse {
            start >= boundary
        } else {
            start < boundary
        } {
            continue;
        }
        if span.end.byte - span.start.byte > 1024 {
            partial = true;
            continue;
        }
        selected.push(span);
    }
    if reverse {
        selected.reverse();
    }
    (selected, partial)
}
