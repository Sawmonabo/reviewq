use std::collections::HashSet;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use reviewq_domain::identity::ReportHash;
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::access::AppError;

use super::{Descriptor, SourceRange};

const MAX_DESCRIPTORS: usize = 4096;
/// Equals the protocol's `limits::MAX_TITLE_BYTES`, so a heading title fits a ledger title.
const MAX_TITLE_BYTES: usize = 384;
/// Equals the protocol's `limits::MAX_SEVERITY_BYTES`.
const MAX_SEVERITY_BYTES: usize = 64;
/// The text kept from the paragraph under a finding heading, which holds its metadata lines.
const MAX_CARD_TEXT_BYTES: usize = 384;
/// The longest category, in UTF-16 units, the reader accepts in a `P1 · Category · blocking`
/// line.
const MAX_CATEGORY_UNITS: usize = 80;
/// The field labels that end a finding card's metadata lines in the reader.
const FIELD_LABELS: [&str; 7] = [
    "Issue",
    "Why it matters",
    "Confirmed",
    "Proposed comment",
    "Location",
    "Evidence",
    "Suggested fix",
];

/// Admits one full Markdown parse at a time in this service.
static ADMISSION: Semaphore = Semaphore::const_new(1);

/// Waits for the parse admission, as acceptance does.
pub(super) async fn admit() -> Result<SemaphorePermit<'static>, AppError> {
    ADMISSION.acquire().await.map_err(|_| AppError::Internal)
}

/// Takes the parse admission only when no other parse holds it, as a submit does.
pub(super) fn try_admit() -> Option<SemaphorePermit<'static>> {
    ADMISSION.try_acquire().ok()
}

struct Heading {
    level: usize,
    offset: usize,
    title: String,
    finding_id: Option<String>,
    fields: Fields,
}

/// What a finding section's metadata lines say, kept by kind until the section ends.
#[derive(Default)]
struct Fields {
    /// The last `Severity:` and `Blocking:` lines, which override the others.
    severity: Option<String>,
    blocking: Option<bool>,
    /// The first `P1 · Category · blocking` lines' priority and blocking status.
    line_severity: Option<String>,
    line_blocking: Option<bool>,
    /// A labelled field such as `**Evidence:**` has begun, which ends those lines.
    labelled: bool,
}

/// A paragraph under a finding heading, with the text of its leading strong span if any.
struct Paragraph {
    text: String,
    started: bool,
    lead: Option<String>,
    lead_depth: usize,
    images: Vec<usize>,
}

/// Indexes the report as the reader draws it: its top-level sections and level-three `F-`
/// finding headings with their metadata lines. Content nested in a quote, list item,
/// footnote or definition is skipped, except that a level-three `F-` heading in a list item
/// outside any quote yields a finding without a span, carrying only its title and heading
/// priority, so it is still tracked. A repeated `F-` ID anchors at its first top-level
/// heading; later ones are plain sections. Returns `None` only when the report has more
/// headings than the index admits.
pub(super) fn parse(source: &str) -> Option<Vec<Descriptor>> {
    let mut headings: Vec<Heading> = Vec::new();
    let mut nested: Vec<(String, String)> = Vec::new();
    let mut heading: Option<(Heading, bool)> = None;
    let mut images = Vec::new();
    // The finding whose card the next paragraph or level-four to six heading joins: the
    // reader's card runs to the next top-level heading of level one to three.
    let mut finding: Option<usize> = None;
    let mut paragraph: Option<Paragraph> = None;
    let mut quotes = 0_u32;
    let mut items = 0_u32;
    // Strikethrough matches the reader's GFM, which drops the tildes from heading text.
    let options = Options::ENABLE_GFM | Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for (event, offset) in Parser::new_ext(source, options).into_offset_iter() {
        if let Some(value) = paragraph.as_mut()
            && !matches!(event, Event::End(TagEnd::Paragraph))
        {
            value.push(&event);
            continue;
        }
        if let Some((value, _)) = heading.as_mut()
            && !matches!(event, Event::End(TagEnd::Heading(_)))
        {
            if let Some(text) = inline_text(&event, &mut images, value.title.len()) {
                limited_append(&mut value.title, text, MAX_TITLE_BYTES);
            }
            continue;
        }
        match event {
            // The other tags whose content holds blocks; nothing inside them is indexed.
            // Footnotes and definition lists are parsed only with their own options, so
            // those arms guard a later option change.
            Event::Start(
                Tag::BlockQuote(_) | Tag::FootnoteDefinition(_) | Tag::DefinitionListDefinition,
            ) => quotes += 1,
            Event::End(
                TagEnd::BlockQuote(_)
                | TagEnd::FootnoteDefinition
                | TagEnd::DefinitionListDefinition,
            ) => quotes -= 1,
            Event::Start(Tag::Item) => items += 1,
            Event::End(TagEnd::Item) => items -= 1,
            Event::Start(Tag::Heading { level, .. })
                if quotes == 0 && (items == 0 || level == HeadingLevel::H3) =>
            {
                let value = Heading {
                    level: level as usize,
                    offset: offset.start,
                    title: String::new(),
                    finding_id: None,
                    fields: Fields::default(),
                };
                heading = Some((value, items != 0));
            }
            Event::End(TagEnd::Heading(_)) => {
                let Some((mut value, in_item)) = heading.take() else {
                    continue;
                };
                let id = (value.level == 3)
                    .then(|| finding_id(&value.title))
                    .flatten();
                if (!in_item || id.is_some()) && headings.len() + nested.len() == MAX_DESCRIPTORS {
                    return None;
                }
                if in_item {
                    if let Some(id) = id {
                        nested.push((id, value.title));
                    }
                    continue;
                }
                if value.level <= 3 {
                    finding = id.is_some().then_some(headings.len());
                } else if let Some(index) = finding
                    && field_label(value.title.trim())
                {
                    // The reader takes a level-four to six field label as a labelled field.
                    headings[index].fields.labelled = true;
                }
                value.finding_id = id;
                headings.push(value);
            }
            _ if quotes + items != 0 => {}
            Event::Start(Tag::Paragraph) if finding.is_some() => {
                paragraph = Some(Paragraph {
                    text: String::new(),
                    started: false,
                    lead: None,
                    lead_depth: 0,
                    images: Vec::new(),
                });
            }
            Event::End(TagEnd::Paragraph) => {
                if let Some(value) = paragraph.take()
                    && let Some(index) = finding
                {
                    headings[index].fields.read(value);
                }
            }
            _ => {}
        }
    }

    let mut ends = vec![source.len(); headings.len()];
    let mut next = [source.len(); 7];
    for (index, heading) in headings.iter().enumerate().rev() {
        ends[index] = next[..=heading.level]
            .iter()
            .copied()
            .min()
            .unwrap_or(source.len());
        next[heading.level] = heading.offset;
    }
    let mut seen = HashSet::new();
    let mut descriptors = Vec::new();
    let mut last_byte = 0;
    let mut scalar = 0;
    for (heading, end) in headings.into_iter().zip(ends) {
        scalar += source[last_byte..heading.offset].chars().count() as u64;
        last_byte = heading.offset;
        let body = &source[heading.offset..end];
        let range = SourceRange {
            offset: scalar,
            length: body.chars().count() as u64,
            hash: ReportHash::from_sha256(Sha256::digest(body.trim_end().as_bytes()).into()),
            origin: None,
        };
        let title = if heading.title.trim().is_empty() {
            "Untitled section".to_owned()
        } else {
            heading.title
        };
        if let Some(id) = heading.finding_id
            && seen.insert(id.clone())
        {
            let fields = heading.fields;
            descriptors.push(Descriptor::Finding {
                id,
                severity: fields
                    .severity
                    .or_else(|| heading_severity(&title))
                    .or(fields.line_severity),
                blocking: fields.blocking.or(fields.line_blocking),
                title: title.clone(),
                source: Some(range.clone()),
                carried_from: None,
                details: None,
                disposition: None,
            });
        }
        descriptors.push(Descriptor::Section {
            title,
            source: range,
            cursor_offset: scalar,
        });
    }
    // The reader reads no metadata lines under a nested heading, so it carries only its title
    // and heading priority.
    for (id, title) in nested {
        if seen.insert(id.clone()) {
            descriptors.push(Descriptor::Finding {
                id,
                severity: heading_severity(&title),
                blocking: None,
                title,
                source: None,
                carried_from: None,
                details: None,
                disposition: None,
            });
        }
    }
    Some(descriptors)
}

impl Paragraph {
    fn push(&mut self, event: &Event<'_>) {
        let first = !self.started;
        self.started = true;
        match event {
            Event::Start(Tag::Strong) if first => {
                self.lead = Some(String::new());
                self.lead_depth = 1;
            }
            Event::Start(Tag::Strong) if self.lead_depth > 0 => self.lead_depth += 1,
            Event::End(TagEnd::Strong) if self.lead_depth > 0 => self.lead_depth -= 1,
            _ => {
                if let Some(text) = inline_text(event, &mut self.images, self.text.len()) {
                    limited_append(&mut self.text, text, MAX_CARD_TEXT_BYTES);
                    if self.lead_depth > 0
                        && let Some(lead) = self.lead.as_mut()
                    {
                        limited_append(lead, text, MAX_CARD_TEXT_BYTES);
                    }
                }
            }
        }
    }
}

impl Fields {
    /// Reads a paragraph as the reader does until the first labelled field: a paragraph that
    /// is a single `Severity:` or `Blocking:` line sets that field, the last one winning.
    /// Otherwise the paragraph or its leading strong span may be the reader's
    /// `P1 · Category · blocking` line, or a field label that ends these lines.
    fn read(&mut self, paragraph: Paragraph) {
        if self.labelled {
            return;
        }
        let text = paragraph.text.trim();
        if let Some(value) = severity_line(text) {
            // The reader allows 64 UTF-16 units; a longer value in bytes cannot be stored.
            if value.len() <= MAX_SEVERITY_BYTES {
                self.severity = Some(value.to_owned());
            }
            return;
        }
        if let Some(value) = blocking_line(text) {
            self.blocking = Some(value);
            return;
        }
        let candidate = paragraph.lead.as_deref().map_or(text, str::trim);
        if let Some((severity, blocking)) = metadata_line(candidate) {
            if self.line_severity.is_none() {
                self.line_severity = severity;
            }
            if self.line_blocking.is_none() {
                self.line_blocking = Some(blocking);
            }
        } else if paragraph.lead.is_some() && field_label(candidate) {
            self.labelled = true;
        }
    }
}

/// The text an inline event adds as the reader's `reportText` keeps it: text and inline
/// code, line breaks as newlines, and `Attachment` for an image without alt text. An HTML
/// tag adds nothing, while the text between tags still counts. `images` holds the text
/// length at each open image.
fn inline_text<'a>(
    event: &'a Event<'_>,
    images: &mut Vec<usize>,
    length: usize,
) -> Option<&'a str> {
    match event {
        Event::Text(text) | Event::Code(text) => Some(text),
        Event::SoftBreak | Event::HardBreak => Some("\n"),
        Event::Start(Tag::Image { .. }) => {
            images.push(length);
            None
        }
        Event::End(TagEnd::Image) => (images.pop() == Some(length)).then_some("Attachment"),
        _ => None,
    }
}

fn limited_append(output: &mut String, text: &str, max: usize) {
    let available = max.saturating_sub(output.len());
    let end = text.floor_char_boundary(available.min(text.len()));
    output.push_str(&text[..end]);
}

/// Strips a label such as `Severity:` from the start of the text, ignoring case.
fn strip_label<'a>(text: &'a str, label: &str) -> Option<&'a str> {
    text.get(..label.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(label))
        .map(|_| &text[label.len()..])
}

/// The reader's `Severity: value` paragraph: one line of at most 64 UTF-16 units after the
/// label, kept as written.
fn severity_line(text: &str) -> Option<&str> {
    let value = strip_label(text, "Severity:")?.trim_start();
    (!value.is_empty() && !value.contains('\n') && value.encode_utf16().count() <= 64)
        .then_some(value)
}

/// The reader's `Blocking: yes` paragraph: `true`, `yes`, `false` or `no`, ignoring case,
/// with an optional trailing `.`.
fn blocking_line(text: &str) -> Option<bool> {
    let value = strip_label(text, "Blocking:")?.trim_start();
    let value = value.strip_suffix('.').unwrap_or(value);
    let is = |word: &str| value.eq_ignore_ascii_case(word);
    if is("true") || is("yes") {
        Some(true)
    } else if is("false") || is("no") {
        Some(false)
    } else {
        None
    }
}

fn finding_id(title: &str) -> Option<String> {
    let suffix = title.trim_start().strip_prefix("F-")?;
    let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0
        || digits > 16
        || suffix[digits..].chars().next().is_some_and(|next| {
            !next.is_whitespace() && !matches!(next, ':' | '.' | '-' | '–' | '—')
        })
    {
        return None;
    }
    Some(format!("F-{}", &suffix[..digits]))
}

/// The priority of a finding heading such as `F-01 · P1 · Title`, as the reader reads it.
fn heading_severity(title: &str) -> Option<String> {
    let suffix = title.trim_start().strip_prefix("F-")?;
    let rest = suffix.trim_start_matches(|next: char| next.is_ascii_digit());
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix(['·', ':', '.', '–', '—', '-'])
        .map_or(rest, str::trim_start);
    let priority = rest
        .get(..2)
        .filter(|value| matches!(value.as_bytes(), [b'P', b'0'..=b'3']))?;
    rest[2..]
        .trim_start()
        .starts_with(['·', '–', '—', '-'])
        .then(|| priority.to_owned())
}

/// Reads the reader's `P1 · Category · blocking` line: an optional priority, a category of
/// at most 80 UTF-16 units, and `blocking` or `nonblocking`, separated by `·`, ignoring case.
/// Returns the priority as written and the blocking status.
fn metadata_line(text: &str) -> Option<(Option<String>, bool)> {
    let text = text.strip_suffix('.').unwrap_or(text);
    let (head, word) = text.rsplit_once('·')?;
    let word = word.trim_start();
    let blocking = if word.eq_ignore_ascii_case("blocking") {
        true
    } else if word.eq_ignore_ascii_case("nonblocking") {
        false
    } else {
        return None;
    };
    let category = |value: &str, leading: bool| {
        let core = if leading {
            value.trim()
        } else {
            value.trim_end()
        };
        if core.is_empty() {
            // A single blank character can stand in for the category.
            value
                .chars()
                .any(|next| next.is_whitespace() && next != '\n')
        } else {
            !core.contains(['·', '\n']) && core.encode_utf16().count() <= MAX_CATEGORY_UNITS
        }
    };
    let priority = head
        .get(..2)
        .filter(|value| matches!(value.as_bytes(), [b'P' | b'p', b'0'..=b'3']));
    if let Some(priority) = priority
        && let Some(rest) = head[2..].trim_start().strip_prefix('·')
        && category(rest, true)
    {
        return Some((Some(priority.to_owned()), blocking));
    }
    (!head.is_empty() && category(head, false)).then_some((None, blocking))
}

/// Whether a leading strong span is one of the reader's field labels, such as `Evidence:`.
fn field_label(text: &str) -> bool {
    FIELD_LABELS.iter().any(|label| {
        text.get(..label.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(label))
            && text[label.len()..]
                .trim_start()
                .chars()
                .next()
                .is_none_or(|next| matches!(next, ':' | '.' | '—'))
    })
}
