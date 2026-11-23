use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use super::{
    Correction, Descriptor, EvidenceTarget, FindingBasisKind, SourceRange, StructuredFinding,
};

pub fn presentation_source<'a>(
    finding: &StructuredFinding,
    correction: &Correction,
    sections: &'a [Descriptor],
) -> Option<&'a SourceRange> {
    let mut matches = sections.iter().filter_map(|entry| match entry {
        Descriptor::Section { title, source, .. }
            if title == &finding.title
                && correction.evidence.iter().any(|reference| {
                    matches!(&reference.target, EvidenceTarget::Section(cited) if cited == title)
                        && reference.hash == source.hash
                }) =>
        {
            Some(source)
        }
        _ => None,
    });
    let source = matches.next()?;
    matches.next().is_none().then_some(source)
}

/// A presentation correction must reformat claims already present in the cited section.
pub fn presentation_matches(markdown: &str, finding: &StructuredFinding) -> bool {
    let mut depth = 0;
    let mut paragraphs = String::new();
    let mut paragraph = false;
    let mut heading = false;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Heading { .. }) if depth == 0 => {
                if heading {
                    break;
                }
                heading = true;
                depth += 1;
            }
            Event::Start(Tag::Paragraph) if depth == 0 => {
                paragraph = true;
                depth += 1;
            }
            Event::Start(_) => depth += 1,
            Event::End(TagEnd::Paragraph) if depth == 1 && paragraph => {
                paragraph = false;
                depth -= 1;
                paragraphs.push_str("\n\n");
            }
            Event::End(_) => depth -= 1,
            Event::Text(text) | Event::Code(text) if paragraph => paragraphs.push_str(&text),
            Event::SoftBreak | Event::HardBreak if paragraph => paragraphs.push('\n'),
            _ => {}
        }
    }
    let explicit = |label| {
        let mut values = paragraphs.lines().filter_map(|line| {
            let (actual, value) = line.trim().split_once(':')?;
            (actual == label).then_some(value.trim())
        });
        let first = values.next()?;
        values.next().is_none().then_some(first)
    };
    let blocking = if finding.blocking { "true" } else { "false" };
    let evidence = match finding.details.evidence.kind {
        FindingBasisKind::Observed => "Evidence",
        FindingBasisKind::Gap => "Evidence gap",
    };
    let consequence = finding.details.consequence.as_str();
    let has_consequence = explicit("Consequence") == Some(consequence)
        || explicit("Why it matters") == Some(consequence)
        || paragraphs
            .split("\n\n")
            .any(|paragraph| paragraph.trim() == consequence);
    explicit("Severity") == Some(finding.severity.as_str())
        && explicit("Blocking") == Some(blocking)
        && explicit(evidence) == Some(finding.details.evidence.text.as_str())
        && has_consequence
        && finding
            .details
            .location
            .as_ref()
            .is_none_or(|location| match location.line {
                Some(line) => {
                    explicit("Location") == Some(format!("{}:{line}", location.path).as_str())
                }
                None => explicit("Location") == Some(location.path.as_str()),
            })
}
