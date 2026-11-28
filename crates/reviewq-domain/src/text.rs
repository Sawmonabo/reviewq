//! Text cut to fit a bounded message.

/// `text` cut to at most `limit` bytes at a character boundary, ending in "…" when cut.
pub fn clip(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        text.truncate(text.floor_char_boundary(limit.saturating_sub('…'.len_utf8())));
        text.push('…');
    }
    text
}
