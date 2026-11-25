//! The four record tools: `record_finding`, `assess_finding`, `record_note` and `review_ledger`.
//! Each checks its Markdown fields with `markdown::check_field`, returns every field error
//! together, then makes one `LedgerStore` call and words a `WriteRefusal` as a field error. The
//! protocol checked field shapes, sizes and titles; the transport caps the error list.
