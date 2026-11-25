//! Composes a version's text from a `LedgerSnapshot` in one pass, recording each card's and
//! section's span as it writes. It parses nothing: fields were checked when filed, and their
//! headings are shifted with `markdown::shift_headings`.
