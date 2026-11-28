//! What the `flow` and `changes` sections must hold, judged as the page draws them.

use satteri_pulldown_cmark::{CodeBlockKind, Event, Tag, TagEnd};

use super::{FieldProblem, Parse, ProblemKind, line_of};
use crate::reports::{diagram_source, is_mermaid, undrawable};

/// The headings of the table of affected operations, as compared: trimmed and lowercased.
const OPERATION_HEADINGS: [&str; 3] = ["affected operation", "what changes", "source"];

/// `Ok` when `text` holds a Mermaid diagram the page draws: a fenced code block at any depth,
/// at the top level or inside a list item or a quote, whose info string `is_mermaid` accepts,
/// whose content is not blank and which `undrawable` passes. The page puts every Mermaid block in
/// a diagram figure (`CodeBlock` in `web/src/components/markdown/MarkdownContent.tsx`) and draws
/// it only when that test passes. Otherwise the problem names, in this order, why the first
/// non-blank Mermaid block is not drawn, the first line of an indented code block that reads as a
/// ```` ```mermaid ```` line, which the page shows as plain code, or that the diagram is missing.
pub(super) fn diagram(text: &str) -> Result<(), FieldProblem> {
    // The open Mermaid block's first line and text so far.
    let mut mermaid: Option<(usize, String)> = None;
    let mut indented = false;
    let mut not_drawn = None;
    let mut fake_fence = None;
    for (event, range) in Parse::new(text).events() {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                mermaid = is_mermaid(&kind).then(|| (line_of(text, range.start), String::new()));
                indented = matches!(kind, CodeBlockKind::Indented);
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((line, source)) = mermaid.take()
                    && !source.trim().is_empty()
                {
                    match undrawable(&diagram_source(&source)) {
                        None => return Ok(()),
                        Some(reason) => {
                            not_drawn = not_drawn.or(Some((line, reason)));
                        }
                    }
                }
                indented = false;
            }
            Event::Text(value) => {
                if let Some((_, source)) = mermaid.as_mut() {
                    source.push_str(&value);
                } else if indented && fake_fence.is_none() {
                    fake_fence = value
                        .split('\n')
                        .position(reads_as_mermaid_fence)
                        .map(|index| line_of(text, range.start) + index);
                }
            }
            _ => {}
        }
    }
    let (line, kind) = match (not_drawn, fake_fence) {
        (Some((line, reason)), _) => (line, ProblemKind::NotDrawn(reason)),
        (None, Some(line)) => (line, ProblemKind::IndentedFence),
        (None, None) => (1, ProblemKind::NoDiagram),
    };
    Err(FieldProblem { line, kind })
}

/// Whether `line` would open a Mermaid fence if it were not indented: ```` ``` ```` or `~~~`,
/// then an info string whose first word is `mermaid`.
fn reads_as_mermaid_fence(line: &str) -> bool {
    let line = line.trim_start();
    let Some(marker @ ('`' | '~')) = line.chars().next() else {
        return false;
    };
    let info = line.trim_start_matches(marker);
    line.len() - info.len() >= 3 && info.trim_start().split([' ', '\t']).next() == Some("mermaid")
}

/// One top-level table: its header cells as compared, trimmed and lowercased, and whether it has
/// a body row.
#[derive(Default)]
struct Table {
    headings: Vec<String>,
    body: bool,
}

impl Table {
    /// Whether the page draws this table as Before and After cards: one header row of exactly
    /// the headings `Before` and `After`, ignoring case and surrounding space, over at least one
    /// body row of two cells. Mirrors `reportTable` and `twoColumnBody` in
    /// `web/src/features/review/report/report-structure.ts`; change both together. A Markdown
    /// table has one header row and no merged cells, and the page gives every body row as many
    /// cells as the header, so a body row is all the rest needs.
    fn is_before_after(&self) -> bool {
        self.headings == ["before", "after"] && self.body
    }

    fn is_operations(&self) -> bool {
        self.headings == OPERATION_HEADINGS
    }
}

/// `Ok` when the top-level tables of `text` hold a Before and After table, then a table of the
/// affected operations headed `Affected operation`, `What changes` and `Source`, ignoring case
/// and surrounding space. Only a top-level table counts, because the page styles only the
/// report's top-level tables. Otherwise the problem names the table that is missing.
pub(super) fn tables(text: &str) -> Result<(), FieldProblem> {
    let tables = top_level_tables(text);
    let missing = match tables.iter().position(Table::is_before_after) {
        None => ProblemKind::NoBeforeAfter,
        Some(index) if !tables[index + 1..].iter().any(Table::is_operations) => {
            ProblemKind::NoOperations
        }
        Some(_) => return Ok(()),
    };
    Err(FieldProblem {
        line: 1,
        kind: missing,
    })
}

/// Every top-level table of `text`, in order. A heading's text is what the page compares: its
/// text, code and raw HTML, which the report shows as written, but not an image's description,
/// which the page keeps out of the text.
fn top_level_tables(text: &str) -> Vec<Table> {
    let mut tables = Vec::new();
    let mut depth = 0_usize;
    let mut in_table = false;
    let mut in_head = false;
    // How many images are open around the current event.
    let mut images = 0_usize;
    for (event, _) in Parse::new(text).events() {
        match &event {
            Event::Start(tag) => {
                match tag {
                    Tag::Table(_) if depth == 0 => {
                        in_table = true;
                        tables.push(Table::default());
                    }
                    Tag::TableHead if in_table => in_head = true,
                    Tag::TableRow if in_table && !in_head => {
                        tables.last_mut().into_iter().for_each(|t| t.body = true);
                    }
                    Tag::TableCell if in_head => {
                        tables
                            .last_mut()
                            .into_iter()
                            .for_each(|t| t.headings.push(String::new()));
                    }
                    Tag::Image { .. } => images += 1,
                    _ => {}
                }
                depth += 1;
            }
            Event::End(tag) => {
                depth = depth.saturating_sub(1);
                match tag {
                    TagEnd::TableHead => in_head = false,
                    TagEnd::Image => images = images.saturating_sub(1),
                    TagEnd::Table if depth == 0 => in_table = false,
                    TagEnd::TableCell if in_head => {
                        if let Some(heading) = tables.last_mut().and_then(|t| t.headings.last_mut())
                        {
                            *heading = heading.trim().to_lowercase();
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(value) | Event::Code(value) | Event::InlineHtml(value)
                if in_head && images == 0 =>
            {
                if let Some(heading) = tables.last_mut().and_then(|t| t.headings.last_mut()) {
                    heading.push_str(value);
                }
            }
            _ => {}
        }
    }
    tables
}
