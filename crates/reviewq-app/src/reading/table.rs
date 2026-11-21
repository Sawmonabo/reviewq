//! Native GFM tables with literal source positions for browser selection.

use std::ops::Range;

use satteri_pulldown_cmark::{Alignment, Event, LinkType, Options, Parser, Tag, TagEnd};

pub const MIN_TABLE_BYTES: usize = 8 * 1024;
const MAX_BYTES: usize = 64 * 1024;
const MAX_COLUMNS: usize = 16;
const MAX_ROWS: usize = 129;
const MAX_EVENTS: usize = 8_000;

pub struct PreparedTable {
    pub start: u32,
    pub end: u32,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Vec<Inline>>>,
}

pub enum Column {
    Default,
    Left,
    Center,
    Right,
}

pub enum Inline {
    Text { start: u32, end: u32 },
    Code { start: u32, end: u32, text: String },
    Emphasis,
    Strong,
    Strike,
    Link { href: String, title: String },
    Close,
}

struct Coordinates<'a> {
    source: &'a str,
    byte: usize,
    utf16: u32,
}

impl Coordinates<'_> {
    fn advance(&mut self, byte: usize) -> Option<u32> {
        self.utf16 += self.source.get(self.byte..byte)?.encode_utf16().count() as u32;
        self.byte = byte;
        Some(self.utf16)
    }

    fn span(&mut self, span: Range<usize>) -> Option<(u32, u32)> {
        Some((self.advance(span.start)?, self.advance(span.end)?))
    }
}

/// The input is one verified top-level block. Unknown markup stays on the canonical path.
pub fn prepare_table(source: &str) -> Option<PreparedTable> {
    if !(MIN_TABLE_BYTES..=MAX_BYTES).contains(&source.len()) || source.contains('\\') {
        return None;
    }
    let parser = Parser::new_ext(
        source,
        Options::ENABLE_GFM | Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    );
    if parser.reference_definitions().iter().next().is_some() {
        return None;
    }
    let mut events = parser.into_offset_iter().enumerate();
    let (_, (Event::Start(Tag::Table(columns)), extent)) = events.next()? else {
        return None;
    };
    if columns.is_empty() || columns.len() > MAX_COLUMNS {
        return None;
    }
    let mut result = PreparedTable {
        start: source.get(..extent.start)?.encode_utf16().count() as u32,
        end: source.get(..extent.end)?.encode_utf16().count() as u32,
        columns: columns
            .into_iter()
            .map(|column| match column {
                Alignment::None => Column::Default,
                Alignment::Left => Column::Left,
                Alignment::Center => Column::Center,
                Alignment::Right => Column::Right,
            })
            .collect(),
        rows: Vec::new(),
    };
    let mut coords = Coordinates {
        source,
        byte: 0,
        utf16: 0,
    };
    let mut row = None::<Vec<Vec<Inline>>>;
    let mut cell = None::<Vec<Inline>>;
    let mut depth = 0;
    while let Some((events_seen, (event, span))) = events.next() {
        if events_seen >= MAX_EVENTS {
            return None;
        }
        match event {
            Event::Start(Tag::TableHead | Tag::TableRow) if row.is_none() => row = Some(Vec::new()),
            Event::Start(Tag::TableCell) if row.is_some() && cell.is_none() => {
                cell = Some(Vec::new())
            }
            Event::End(TagEnd::TableCell) if depth == 0 => {
                let current = row.as_mut()?;
                current.push(cell.take()?);
                if current.len() > result.columns.len() {
                    return None;
                }
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) if cell.is_none() => {
                let mut current = row.take()?;
                current.resize_with(result.columns.len(), Vec::new);
                result.rows.push(current);
                if result.rows.len() > MAX_ROWS {
                    return None;
                }
            }
            Event::End(TagEnd::Table) if row.is_none() && cell.is_none() => {
                return (result.rows.len() > 1 && events.next().is_none()).then_some(result);
            }
            Event::Text(value) => {
                let raw = source.get(span.clone())?;
                // Literal autolinks, HTML, escapes and decoded entities can differ
                // between the two parsers or require richer source coordinates.
                if value.as_ref() != raw
                    || raw.contains(['<', '@'])
                    || raw.contains("://")
                    || raw.contains("www.")
                {
                    return None;
                }
                let (start, end) = coords.span(span)?;
                let runs = cell.as_mut()?;
                if let Some(Inline::Text { end: previous, .. }) = runs.last_mut()
                    && *previous == start
                {
                    *previous = end;
                } else {
                    runs.push(Inline::Text { start, end });
                }
            }
            Event::Code(text) => {
                let (start, end) = coords.span(span)?;
                cell.as_mut()?.push(Inline::Code {
                    start,
                    end,
                    text: text.into_string(),
                });
            }
            Event::Start(tag) => {
                depth += 1;
                if depth > 16 {
                    return None;
                }
                cell.as_mut()?.push(match tag {
                    Tag::Emphasis => Inline::Emphasis,
                    Tag::Strong => Inline::Strong,
                    Tag::Strikethrough => Inline::Strike,
                    Tag::Link {
                        link_type: LinkType::Inline,
                        dest_url,
                        title,
                        ..
                    } if supported_url(&dest_url) => Inline::Link {
                        href: dest_url.into_string(),
                        title: title.into_string(),
                    },
                    _ => return None,
                });
            }
            Event::End(
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link,
            ) if depth > 0 => {
                depth -= 1;
                cell.as_mut()?.push(Inline::Close);
            }
            _ => return None,
        }
    }
    None
}

fn supported_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://") || url.starts_with('#'))
        && url
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~:/?#&=+".contains(&byte))
}
