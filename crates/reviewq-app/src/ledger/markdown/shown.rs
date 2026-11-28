//! Inline text as the report page shows it.

use satteri_pulldown_cmark::{Event, Tag, TagEnd};

/// What the page shows for an image, with no alt text: `imageLabel` in
/// `web/src/components/markdown/parse-markdown.ts`.
const UNNAMED_IMAGE: &str = "Attachment";

/// The text of a run of inline events as the page reads it with `reportText` in
/// `web/src/features/review/report/report-structure.ts`, after `parseMarkdown` in literal mode
/// (`reportMarkdown`) has built the tree. Change both together. Text and code are shown as
/// written. Raw HTML is shown as written too, except that each `<br>`, `<br/>`, `<br />` or
/// `<br >`, in that case, is a line break, which the page shows as `\n`, as it does a soft or
/// hard break. An image shows its alt text, or `Attachment` when it has none. The alt text is
/// the image's own text, code and raw HTML as written, with `\n` for a soft break and nothing
/// for a hard one, read before the HTML is turned into text.
///
/// Where a line ends inside the text the two parsers may differ in the spaces around it, so
/// a caller compares only text that cannot hold a line ending, such as a table cell, or
/// text whose test fails on any line break, such as the counts line.
#[derive(Default)]
pub(super) struct ShownText {
    text: String,
    /// Images open around the current event.
    images: usize,
    /// The outermost open image's alt text so far.
    alt: String,
}

impl ShownText {
    /// Adds what `event` shows. Every other event, such as the start of emphasis or a link,
    /// shows nothing of its own.
    pub(super) fn push(&mut self, event: &Event<'_>) {
        let in_image = self.images > 0;
        match event {
            Event::Start(Tag::Image { .. }) => self.images += 1,
            Event::End(TagEnd::Image) => {
                self.images = self.images.saturating_sub(1);
                if self.images == 0 {
                    let alt = std::mem::take(&mut self.alt);
                    self.text
                        .push_str(if alt.is_empty() { UNNAMED_IMAGE } else { &alt });
                }
            }
            Event::Text(value) | Event::Code(value) if in_image => self.alt.push_str(value),
            Event::Text(value) | Event::Code(value) => self.text.push_str(value),
            Event::Html(value) | Event::InlineHtml(value) if in_image => self.alt.push_str(value),
            Event::Html(value) | Event::InlineHtml(value) => literal_html(&mut self.text, value),
            Event::SoftBreak if in_image => self.alt.push('\n'),
            Event::SoftBreak | Event::HardBreak if !in_image => self.text.push('\n'),
            _ => {}
        }
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }

    pub(super) fn into_string(self) -> String {
        self.text
    }
}

/// Writes `html` as the page shows raw HTML in report text: as written, with each match of
/// `/<br ?\/?>/g` (`lineBreak` in `parse-markdown.ts`) as `\n`.
fn literal_html(text: &mut String, html: &str) {
    let mut rest = html;
    while let Some(at) = rest.find("<br") {
        text.push_str(&rest[..at]);
        let after = &rest[at + "<br".len()..];
        let tail = after.strip_prefix(' ').unwrap_or(after);
        let tail = tail.strip_prefix('/').unwrap_or(tail);
        if let Some(end) = tail.strip_prefix('>') {
            text.push('\n');
            rest = end;
        } else {
            text.push_str("<br");
            rest = after;
        }
    }
    text.push_str(rest);
}
