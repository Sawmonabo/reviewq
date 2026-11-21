//! Project the same authorized Markdown selection that the browser will open.

use crate::history::document::{Window, WindowResult};
use crate::reading::displayed_markdown;

use super::DocumentMatch;

pub struct Projection {
    pub proof: DocumentMatch,
    pub text: Option<String>,
    pub previous: Option<u64>,
    pub next: Option<u64>,
}

pub fn project(window: Window) -> Projection {
    let Window {
        hash,
        bytes,
        previous,
        next,
        result,
        ..
    } = window;
    let text = match result {
        WindowResult::Ready(blocks) => {
            let source: String = blocks.into_iter().map(|block| block.markdown).collect();
            displayed_markdown(&source)
        }
        _ => None,
    };
    Projection {
        proof: DocumentMatch {
            hash,
            byte_start: bytes.start,
            canonical: false,
        },
        text,
        previous,
        next,
    }
}
