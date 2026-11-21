//! Search the text a reader can display and retain its original coordinates.

use icu_casemap::CaseMapper;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

mod markdown;

pub use markdown::{MAX_DISCUSSION_TEXT_SOURCE_BYTES, discussion_text, displayed_markdown};

const MAX_LITERAL_BYTES: usize = 256;

pub struct Literal(String);

impl Literal {
    pub fn new(query: &str) -> Option<Self> {
        if query.is_empty() || query.len() > MAX_LITERAL_BYTES {
            return None;
        }
        let folded = fold(query);
        (!folded.is_empty() && folded.len() <= MAX_LITERAL_BYTES).then_some(Self(folded))
    }

    pub fn folded(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextPoint {
    pub byte: usize,
    pub utf16: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextMatch {
    pub start: TextPoint,
    pub end: TextPoint,
}

struct Boundary {
    folded: usize,
    original: TextPoint,
}

/// Translation from folded offsets back to boundaries in the displayed text.
pub struct FoldedText {
    text: String,
    boundaries: Vec<Boundary>,
}

impl FoldedText {
    pub fn new(original: &str) -> Self {
        let mut text = String::with_capacity(original.len());
        let mut boundaries = Vec::new();
        let mut utf16 = 0;
        for (byte, piece) in original.grapheme_indices(true) {
            boundaries.push(Boundary {
                folded: text.len(),
                original: TextPoint { byte, utf16 },
            });
            if piece.is_ascii() {
                text.extend(piece.chars().map(|value| value.to_ascii_lowercase()));
                utf16 += piece.len();
            } else {
                text.push_str(&fold(piece));
                utf16 += piece.encode_utf16().count();
            }
        }
        boundaries.push(Boundary {
            folded: text.len(),
            original: TextPoint {
                byte: original.len(),
                utf16,
            },
        });
        Self { text, boundaries }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Keep one provisional grapheme and enough complete folded text to cross the cut.
    pub fn settled_prefix(&self, carry_bytes: usize) -> TextPoint {
        let settled = &self.boundaries[self.boundaries.len().saturating_sub(2)];
        let end = settled.folded.saturating_sub(carry_bytes);
        let boundary = self.boundaries.partition_point(|point| point.folded <= end) - 1;
        self.boundaries[boundary].original
    }

    pub fn occurrences<'a>(&'a self, query: &'a Literal) -> impl Iterator<Item = TextMatch> + 'a {
        self.boundaries.iter().filter_map(move |start| {
            self.text[start.folded..]
                .starts_with(query.folded())
                .then_some(())?;
            let end = self
                .boundaries
                .binary_search_by_key(&(start.folded + query.folded().len()), |point| point.folded)
                .ok()?;
            Some(TextMatch {
                start: start.original,
                end: self.boundaries[end].original,
            })
        })
    }
}

fn fold(value: &str) -> String {
    if value.is_ascii() {
        return value.to_ascii_lowercase();
    }
    let composed = value.nfc().collect::<String>();
    CaseMapper::new().fold_string(&composed).nfc().collect()
}
