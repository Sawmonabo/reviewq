//! Text cut to fit a bounded message.

/// `text` cut to at most `limit` bytes at a character boundary, ending in "…" when cut and
/// `limit` leaves room for it.
pub fn clip(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        let mark = '…'.len_utf8();
        let room = limit >= mark;
        text.truncate(text.floor_char_boundary(if room { limit - mark } else { limit }));
        if room {
            text.push('…');
        }
    }
    text
}
