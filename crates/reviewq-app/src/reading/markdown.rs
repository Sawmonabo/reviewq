use std::cell::RefCell;

use html5ever::buffer_queue::BufferQueue;
use html5ever::tokenizer::{TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use markdown::{ParseOptions, mdast::Node};

mod html;

pub const MAX_DISCUSSION_TEXT_SOURCE_BYTES: usize = 32 * 1024;

const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_LINES: usize = 10_000;
const MAX_SYNTAX_MARKS: usize = 1024;
const MAX_BLOCK_PREFIX: usize = 128;
const MAX_DEPTH: usize = 128;
const MAX_NODES: usize = 60_000;

/// Text of one admitted Markdown body. Unknown display constructs have no certified projection.
pub fn displayed_markdown(source: &str) -> Option<String> {
    if source.len() > 65_536
        || source
            .split_terminator('\n')
            .enumerate()
            .any(|(index, line)| index >= MAX_LINES || !admit_line(line))
    {
        return None;
    }
    if source.contains('<') || source.contains("[^") {
        return html::project(source, MAX_DEPTH, MAX_NODES);
    }
    project(source, true)
}

/// Optional plain text for a distant note. The browser retains its existing projection for
/// inputs outside this bounded subset and always renders the original Markdown when reading.
pub fn discussion_text(source: &str) -> Option<String> {
    if source.len() > MAX_DISCUSSION_TEXT_SOURCE_BYTES
        || source.contains(['\0', '\r', '\t'])
        || !looks_like_markdown(source)
        || source
            .split_terminator('\n')
            .enumerate()
            .any(|(index, line)| index >= MAX_LINES || !admit_line(line))
        || source
            .bytes()
            .filter(|byte| b"[]<>\x60~_*".contains(byte))
            .nth(MAX_SYNTAX_MARKS)
            .is_some()
    {
        return None;
    }

    let text = project(source, false)?;
    (!text.eq(source) && text.len() <= MAX_DISCUSSION_TEXT_SOURCE_BYTES).then_some(text)
}

fn project(source: &str, reader: bool) -> Option<String> {
    let tree = markdown::to_mdast(source, &ParseOptions::gfm()).ok()?;
    let mut text = String::new();
    let mut remaining = MAX_NODES;
    append_text(&tree, source, &mut text, 0, &mut remaining, reader)?;
    Some(text)
}

fn admit_line(line: &str) -> bool {
    // Bound list and quote nesting before constructing the Markdown tree.
    line.len() <= MAX_LINE_BYTES
        && line
            .bytes()
            .take_while(|byte| b" >*+-0123456789.)".contains(byte))
            .nth(MAX_BLOCK_PREFIX)
            .is_none()
}

// The compact browser view displays plain input verbatim when these markers are absent.
fn looks_like_markdown(source: &str) -> bool {
    source
        .bytes()
        .any(|byte| b"*_\x60~[<>&\\|#".contains(&byte))
        || source
            .split(['\n', '\r', '\u{2028}', '\u{2029}'])
            .any(|line| {
                let bullet = line.trim_start_matches([' ', '\t']).as_bytes();
                let digits = line.bytes().take_while(u8::is_ascii_digit).count();
                matches!(bullet, [b'+' | b'-', b' ' | b'\t', ..])
                    || (digits > 0
                        && matches!(&line.as_bytes()[digits..], [b'.' | b')', b' ' | b'\t', ..]))
            })
}

fn append_text(
    node: &Node,
    source: &str,
    text: &mut String,
    depth: usize,
    remaining: &mut usize,
    reader: bool,
) -> Option<()> {
    *remaining = remaining.checked_sub(1)?;
    if depth > MAX_DEPTH {
        return None;
    }

    match node {
        Node::Text(content) => text.push_str(&content.value),
        Node::InlineCode(content) => {
            let position = content.position.as_ref()?;
            if !reader && position.start.line != position.end.line {
                return None;
            }
            text.push_str(&content.value);
        }
        Node::Code(content) if !reader || content.lang.as_deref() != Some("mermaid") => {
            text.push_str(&content.value);
        }
        Node::Code(_) => (),
        Node::Image(image) => text.push_str(image_label(node, source, &image.alt)?),
        Node::ImageReference(image) => text.push_str(image_label(node, source, &image.alt)?),
        Node::Break(_) => text.push('\n'),
        Node::Html(html) => text.push_str(&html_text(&html.value)?),
        Node::TableRow(row) if reader => {
            for (index, cell) in row.children.iter().enumerate() {
                if index > 0 {
                    text.push('\t');
                }
                append_text(cell, source, text, depth + 1, remaining, reader)?;
            }
        }
        _ => {
            let separator = match node {
                Node::TableRow(_) => "\t",
                Node::Root(_)
                | Node::List(_)
                | Node::ListItem(_)
                | Node::Blockquote(_)
                | Node::Table(_) => "\n",
                _ => "",
            };
            let start = text.len();
            for child in node.children().into_iter().flatten() {
                let before = text.len();
                if before > start {
                    text.push_str(separator);
                }
                let body = text.len();
                append_text(child, source, text, depth + 1, remaining, reader)?;
                if text.len() == body {
                    text.truncate(before);
                }
            }
        }
    }
    Some(())
}

fn image_label<'a>(node: &Node, source: &str, alt: &'a str) -> Option<&'a str> {
    let position = node.position()?;
    let markup = source.get(position.start.offset..position.end.offset)?;
    markup
        .match_indices("![")
        .nth(1)
        .is_none()
        .then(|| attachment_label(alt))
}

fn attachment_label(alt: &str) -> &str {
    if alt.is_empty() { "Attachment" } else { alt }
}

struct PlainHtml(RefCell<Option<String>>);

impl TokenSink for PlainHtml {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut output = self.0.borrow_mut();
        let Some(text) = output.as_mut() else {
            return TokenSinkResult::Continue;
        };
        match token {
            Token::CharacterTokens(chars) => text.push_str(&chars),
            Token::EOFToken | Token::CommentToken(_) => (),
            // These tags leave text order intact in an inert HTML template. Delegate tags
            // such as tables, templates, scripts and foreign markup to the browser.
            Token::TagToken(tag) => match &*tag.name {
                "a" | "b" | "blockquote" | "br" | "code" | "del" | "details" | "div" | "em"
                | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "i" | "kbd" | "mark" | "p" | "pre"
                | "picture" | "samp" | "small" | "source" | "span" | "strong" | "sub"
                | "summary" | "sup" => (),
                "img" if tag.kind == TagKind::StartTag => {
                    let alt = tag.attrs.iter().find(|attr| &*attr.name.local == "alt");
                    text.push_str(attachment_label(alt.map_or("", |attr| &attr.value)));
                }
                "img" => (),
                _ => *output = None,
            },
            _ => *output = None,
        }
        TokenSinkResult::Continue
    }
}

fn html_text(source: &str) -> Option<String> {
    let stream = BufferQueue::default();
    stream.push_back(source.into());
    let tokenizer = Tokenizer::new(
        PlainHtml(RefCell::new(Some(String::new()))),
        TokenizerOpts {
            discard_bom: false,
            exact_errors: true,
            ..TokenizerOpts::default()
        },
    );
    let _ = tokenizer.feed(&stream);
    tokenizer.end();
    tokenizer.sink.0.into_inner()
}
