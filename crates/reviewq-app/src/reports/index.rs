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
/// The longest category the reader accepts in a `P1 · Category · blocking` line.
const MAX_CATEGORY_CHARS: usize = 80;
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
    /// The last `Severity:` line.
    severity: Option<String>,
    blocking: Explicit,
    /// The first `P1 · Category · blocking` line's priority and blocking status.
    line_severity: Option<String>,
    line_blocking: Option<bool>,
    /// A labelled field such as `**Evidence:**` has begun, which ends those lines.
    labelled: bool,
}

/// The section's `Blocking:` lines. Once two disagree, the field stays unknown.
#[derive(Default)]
enum Explicit {
    #[default]
    Unset,
    Set(bool),
    Conflict,
}

/// A paragraph under a finding heading. `text` is `None` once inline code or HTML makes
/// the paragraph unusable for metadata, and `lead` is `None` once it does so to a leading
/// strong span.
struct Paragraph {
    text: Option<String>,
    started: bool,
    leading: bool,
    lead: Option<String>,
    lead_depth: usize,
}

/// Indexes the report as the reader draws it: its top-level sections and level-three `F-`
/// finding headings with their metadata lines. Content nested in a quote, list item,
/// footnote or definition is skipped, except that a level-three `F-` heading in a list item
/// outside any quote yields a finding without a span, so it is still tracked. A repeated
/// `F-` ID anchors at its first top-level heading; later ones are plain sections. Returns
/// `None` only when the report has more headings than the index admits.
pub(super) fn parse(source: &str) -> Option<Vec<Descriptor>> {
    let mut headings: Vec<Heading> = Vec::new();
    let mut nested: Vec<(String, String)> = Vec::new();
    let mut heading: Option<(Heading, bool)> = None;
    let mut paragraph: Option<Paragraph> = None;
    let mut quotes = 0_u32;
    let mut items = 0_u32;
    for (event, offset) in
        Parser::new_ext(source, Options::ENABLE_GFM | Options::ENABLE_TABLES).into_offset_iter()
    {
        if let Some(value) = paragraph.as_mut()
            && !matches!(event, Event::End(TagEnd::Paragraph))
        {
            value.push(&event);
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
                if !in_item {
                    value.finding_id = id;
                    headings.push(value);
                } else if let Some(id) = id {
                    nested.push((id, value.title));
                }
            }
            Event::Text(text) if heading.is_some() => {
                if let Some((value, _)) = heading.as_mut() {
                    limited_append(&mut value.title, &text, MAX_TITLE_BYTES);
                }
            }
            _ if quotes + items != 0 => {}
            Event::Start(Tag::Paragraph)
                if headings
                    .last()
                    .is_some_and(|value| value.finding_id.is_some()) =>
            {
                paragraph = Some(Paragraph {
                    text: Some(String::new()),
                    started: false,
                    leading: false,
                    lead: None,
                    lead_depth: 0,
                });
            }
            Event::End(TagEnd::Paragraph) => {
                if let Some(value) = paragraph.take()
                    && let Some(heading) = headings.last_mut()
                {
                    heading.fields.read(value);
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
                blocking: match fields.blocking {
                    Explicit::Set(value) => Some(value),
                    Explicit::Conflict => None,
                    Explicit::Unset => fields.line_blocking,
                },
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
                self.leading = true;
                self.lead = Some(String::new());
                self.lead_depth = 1;
            }
            Event::Start(Tag::Strong) if self.lead_depth > 0 => self.lead_depth += 1,
            Event::End(TagEnd::Strong) if self.lead_depth > 0 => self.lead_depth -= 1,
            Event::Text(text) => self.append(text),
            Event::SoftBreak | Event::HardBreak => self.append("\n"),
            Event::Code(_) | Event::InlineHtml(_) => {
                self.text = None;
                if self.lead_depth > 0 {
                    self.lead = None;
                }
            }
            _ => {}
        }
    }

    fn append(&mut self, text: &str) {
        if let Some(value) = self.text.as_mut() {
            limited_append(value, text, MAX_CARD_TEXT_BYTES);
        }
        if self.lead_depth > 0
            && let Some(value) = self.lead.as_mut()
        {
            limited_append(value, text, MAX_CARD_TEXT_BYTES);
        }
    }
}

impl Fields {
    /// Reads `Severity:` and `Blocking:` lines anywhere in the paragraph. Before the first
    /// labelled field, a paragraph or its leading strong span may instead be the reader's
    /// `P1 · Category · blocking` line.
    fn read(&mut self, paragraph: Paragraph) {
        if let Some(text) = &paragraph.text {
            for line in text.lines().map(str::trim) {
                if let Some(value) = line.strip_prefix("Blocking:").map(str::trim) {
                    let blocking = match value {
                        "true" | "yes" => true,
                        "false" | "no" => false,
                        _ => continue,
                    };
                    self.blocking = match self.blocking {
                        Explicit::Unset => Explicit::Set(blocking),
                        Explicit::Set(previous) if previous == blocking => Explicit::Set(blocking),
                        Explicit::Set(_) | Explicit::Conflict => Explicit::Conflict,
                    };
                }
                if let Some(value) = line.strip_prefix("Severity:").map(str::trim)
                    && !value.is_empty()
                    && value.len() <= MAX_SEVERITY_BYTES
                {
                    self.severity = Some(value.to_owned());
                }
            }
        }
        if self.labelled {
            return;
        }
        let candidate = if paragraph.leading {
            paragraph.lead
        } else {
            paragraph.text
        };
        let Some(candidate) = candidate else {
            return;
        };
        let candidate = candidate.trim();
        if let Some((severity, blocking)) = metadata_line(candidate) {
            if self.line_severity.is_none() {
                self.line_severity = severity;
            }
            if self.line_blocking.is_none() {
                self.line_blocking = Some(blocking);
            }
        } else if paragraph.leading && field_label(candidate) {
            self.labelled = true;
        }
    }
}

fn limited_append(output: &mut String, text: &str, max: usize) {
    let available = max.saturating_sub(output.len());
    let end = text.floor_char_boundary(available.min(text.len()));
    output.push_str(&text[..end]);
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
/// at most 80 characters, and `blocking` or `nonblocking`, separated by `·`, ignoring case.
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
            !core.contains(['·', '\n']) && core.chars().count() <= MAX_CATEGORY_CHARS
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
