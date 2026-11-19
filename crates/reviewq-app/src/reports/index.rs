use std::collections::HashSet;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use reviewq_domain::identity::ReportHash;
use sha2::{Digest, Sha256};

use crate::access::AppError;

use super::{Descriptor, SourceRange};

const MAX_DESCRIPTORS: usize = 4096;

struct Heading {
    level: usize,
    offset: usize,
    title: String,
    finding_id: Option<String>,
    severity: Option<String>,
    blocking: Option<bool>,
}

pub(super) fn parse(source: &str) -> Result<Option<Vec<Descriptor>>, AppError> {
    let mut headings = Vec::new();
    let mut heading = None;
    let mut paragraph = None;
    let mut quote_depth = 0_u32;
    for (event, offset) in
        Parser::new_ext(source, Options::ENABLE_GFM | Options::ENABLE_TABLES).into_offset_iter()
    {
        match event {
            Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
            Event::End(TagEnd::BlockQuote(_)) => quote_depth -= 1,
            _ if quote_depth != 0 => {}
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(Heading {
                    level: level as usize,
                    offset: offset.start,
                    title: String::new(),
                    finding_id: None,
                    severity: None,
                    blocking: None,
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                if headings.len() == MAX_DESCRIPTORS {
                    return Ok(None);
                }
                let mut value = heading.take().ok_or(AppError::Internal)?;
                if value.level == 3 {
                    value.finding_id = finding_id(&value.title);
                }
                headings.push(value);
            }
            Event::Start(Tag::Paragraph)
                if headings
                    .last()
                    .is_some_and(|value| value.finding_id.is_some()) =>
            {
                paragraph = Some(String::new());
            }
            Event::End(TagEnd::Paragraph) => {
                if let Some(text) = paragraph.take()
                    && let Some(value) = headings.last_mut()
                {
                    metadata(value, &text)?;
                }
            }
            Event::Text(text) => {
                if let Some(value) = heading.as_mut() {
                    limited_append(&mut value.title, &text, 384);
                }
                if let Some(value) = paragraph.as_mut() {
                    limited_append(value, &text, 384);
                }
            }
            Event::Code(_) | Event::InlineHtml(_) => {
                paragraph = None;
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(value) = paragraph.as_mut() {
                    limited_append(value, "\n", 384);
                }
            }
            _ => {}
        }
    }

    let mut seen = HashSet::new();
    let mut ends = vec![source.len(); headings.len()];
    let mut next = [source.len(); 7];
    for (index, heading) in headings.iter().enumerate().rev() {
        ends[index] = next[..=heading.level]
            .iter()
            .copied()
            .min()
            .ok_or(AppError::Internal)?;
        next[heading.level] = heading.offset;
    }
    let mut items = Vec::new();
    let mut last_byte = 0;
    let mut scalar = 0;
    for (heading, end) in headings.into_iter().zip(ends) {
        scalar += source[last_byte..heading.offset].chars().count() as u64;
        last_byte = heading.offset;
        let body = &source[heading.offset..end];
        let range = || -> Result<SourceRange, AppError> {
            let hash = ReportHash::from_sha256(Sha256::digest(body.trim_end().as_bytes()).into());
            Ok(SourceRange {
                offset: scalar,
                length: body.chars().count() as u64,
                hash,
            })
        };
        let title = if heading.title.trim().is_empty() {
            "Untitled section".to_owned()
        } else {
            heading.title
        };
        if let Some(id) = heading.finding_id {
            if !seen.insert(id.clone()) {
                return Err(AppError::Conflict);
            }
            items.push(Descriptor::Finding {
                id,
                title: title.clone(),
                severity: heading.severity,
                blocking: heading.blocking,
                source: Some(range()?),
            });
        }
        items.push(Descriptor::Section {
            title,
            source: range()?,
        });
    }
    Ok(Some(items))
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

fn metadata(heading: &mut Heading, text: &str) -> Result<(), AppError> {
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("Blocking:").map(str::trim) {
            let blocking = match value {
                "true" | "yes" => true,
                "false" | "no" => false,
                _ => continue,
            };
            if heading
                .blocking
                .is_some_and(|previous| previous != blocking)
            {
                return Err(AppError::Conflict);
            }
            heading.blocking = Some(blocking);
        }
        if let Some(value) = line.strip_prefix("Severity:").map(str::trim)
            && !value.is_empty()
            && value.len() <= 64
        {
            heading.severity = Some(value.to_owned());
        }
    }
    Ok(())
}
