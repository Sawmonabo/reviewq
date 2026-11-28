//! The leading Markdown of each finding card's card line and Location field. The findings
//! list reads it with the browser's own card parser, so its rows say what the cards say
//! without reading the report's text.

use satteri_pulldown_cmark::{Event, Parser, Tag, TagEnd};

use super::{DisplayBlock, DisplayBlockKind, MAX_DISPLAY_HEADING_BYTES};
use crate::reports::REPORT_MARKDOWN;

/// The composer writes each card as `### F-01 · {title}`, then the card line
/// `**P1 · {category} · Blocking**`, then, when the finding has locations,
/// `**Location:** {first}, {second}…` with each location a link or code span.
const CARD_HEADING: &str = "### F-";
const SEPARATOR: &str = " · ";
const LOCATION: &str = "**Location:** ";

/// Gives each card's card line its whole text, and its Location field the text through its
/// first location. A card whose text does not fit `MAX_DISPLAY_HEADING_BYTES` keeps none.
pub(super) fn mark(source: &str, blocks: &mut [DisplayBlock]) {
    for index in 0..blocks.len().saturating_sub(1) {
        if !blocks[index]
            .heading_markdown
            .as_deref()
            .is_some_and(card_heading)
        {
            continue;
        }
        let Some(line) = paragraph(source, &blocks[index + 1]).map(str::trim_end) else {
            continue;
        };
        if line.contains('\n') || !line.starts_with("**P") || !line.ends_with("**") {
            continue;
        }
        let location = blocks
            .get(index + 2)
            .and_then(|block| paragraph(source, block))
            .filter(|text| text.starts_with(LOCATION))
            .map(first_location);
        if line.len() > MAX_DISPLAY_HEADING_BYTES
            || location
                .is_some_and(|text| text.is_none_or(|text| text.len() > MAX_DISPLAY_HEADING_BYTES))
        {
            continue;
        }
        blocks[index + 1].card_markdown = Some(line.to_owned());
        if let Some(Some(text)) = location {
            blocks[index + 2].card_markdown = Some(text.to_owned());
        }
    }
}

/// `### F-01 · …`: a level-three heading whose ID has at least two digits.
fn card_heading(markdown: &str) -> bool {
    markdown.strip_prefix(CARD_HEADING).is_some_and(|rest| {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits >= 2 && rest[digits..].starts_with(SEPARATOR)
    })
}

fn paragraph<'a>(source: &'a str, block: &DisplayBlock) -> Option<&'a str> {
    (block.kind == DisplayBlockKind::Paragraph)
        .then(|| source.get(block.byte_start as usize..block.byte_end as usize))
        .flatten()
}

/// The field through the end of its first link or code span after the bold label, or `None`
/// when it has neither.
fn first_location(field: &str) -> Option<&str> {
    let mut labelled = false;
    for (event, range) in Parser::new_ext(field, REPORT_MARKDOWN).into_offset_iter() {
        match event {
            Event::End(TagEnd::Strong) => labelled = true,
            Event::Start(Tag::Link { .. }) | Event::Code(_) if labelled => {
                return field.get(..range.end);
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::{DisplayIndex, build_report_display_index};

    /// The composer's worked report, which its own tests keep equal to what it writes: a card
    /// label it renames would otherwise silently drop every card's text from the layout.
    const WORKED: &str = include_str!("../../../../../fixtures/reviews/ledger/worked.md");

    #[test]
    fn every_composed_card_carries_its_card_line_and_first_location() {
        let DisplayIndex::Indexed(layout) = build_report_display_index(WORKED) else {
            panic!("the worked report keeps the windowed reader");
        };
        let carried: Vec<(&str, Vec<&str>)> = layout
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(index, block)| {
                let heading = block.heading_markdown.as_deref()?.strip_prefix("### F-")?;
                let cards = layout.blocks[index + 1..=index + 2]
                    .iter()
                    .filter_map(|block| block.card_markdown.as_deref())
                    .collect();
                Some((&heading[..2], cards))
            })
            .collect();
        let blob = "https://gitlab.example.com/acme/billing/-/blob/4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a";
        assert_eq!(
            carried,
            [
                (
                    "01",
                    vec![
                        "**P1 · Functional · Blocking**",
                        &format!("**Location:** [invoices.rs:74]({blob}/src/invoices.rs#L74)"),
                    ]
                ),
                ("04", vec!["**P3 · Quality · Blocking**"]),
                (
                    "02",
                    vec![
                        "**P1 · Reliability · Nonblocking**",
                        &format!("**Location:** [retry.rs:12]({blob}/src/retry.rs#L12)"),
                    ]
                ),
            ]
        );
    }
}
