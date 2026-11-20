//! Persist and validate the source coordinates of each settled display span.

use reviewq_app::access::AppError;
use reviewq_app::reports::{DisplayBlock, DisplayBlockKind, MAX_DISPLAY_HEADING_BYTES};
use rusqlite::{Connection, Row, params};
use sha2::{Digest, Sha256};

use crate::StoreError;

pub(super) const WINDOW_BYTES: u32 = 64 * 1024;
pub(super) const SPAN_COLUMNS: &str = "byte_start, byte_end, scalar_start, utf16_start, kind, heading_level, heading_markdown, checksum";

pub(super) fn write_spans(
    connection: &Connection,
    id: i64,
    report_hash: &str,
    blocks: Vec<DisplayBlock>,
) -> Result<(), StoreError> {
    let sql = format!(
        "INSERT INTO display_spans(entry_id,{SPAN_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
    );
    let mut insert = connection.prepare_cached(&sql)?;
    for block in blocks {
        let (kind, level) = block_label(block.kind);
        let hash = checksum(report_hash, &block);
        insert.execute(params![
            id,
            block.byte_start,
            block.byte_end,
            block.scalar_start,
            block.utf16_start,
            kind,
            level,
            block.heading_markdown,
            hash.as_slice(),
        ])?;
    }
    Ok(())
}

pub(super) fn read_span(row: &Row<'_>, report_hash: &str) -> Result<DisplayBlock, StoreError> {
    let kind = parse_kind(
        row.get_ref(4)?.as_str().map_err(rusqlite::Error::from)?,
        row.get(5)?,
    )?;
    let heading = row
        .get_ref(6)?
        .as_str_or_null()
        .map_err(rusqlite::Error::from)?;
    match (kind, heading) {
        (DisplayBlockKind::Heading(1..=3), Some(markdown))
            if !markdown.is_empty() && markdown.len() <= MAX_DISPLAY_HEADING_BYTES => {}
        (DisplayBlockKind::Heading(1..=3), _) | (_, Some(_)) => {
            return Err(AppError::Internal.into());
        }
        _ => {}
    }
    let span = DisplayBlock {
        byte_start: row.get(0)?,
        byte_end: row.get(1)?,
        scalar_start: row.get(2)?,
        utf16_start: row.get(3)?,
        kind,
        heading_markdown: heading.map(str::to_owned),
    };
    if span.byte_end <= span.byte_start
        || span.byte_end - span.byte_start > WINDOW_BYTES
        || span.scalar_start > span.utf16_start
        || span.utf16_start > span.byte_start
        || checksum(report_hash, &span).as_slice()
            != row.get_ref(7)?.as_blob().map_err(rusqlite::Error::from)?
    {
        return Err(AppError::Internal.into());
    }
    Ok(span)
}

fn checksum(report_hash: &str, span: &DisplayBlock) -> [u8; 32] {
    let (kind, level) = block_label(span.kind);
    Sha256::new()
        .chain_update(report_hash.as_bytes())
        .chain_update(span.byte_start.to_le_bytes())
        .chain_update(span.byte_end.to_le_bytes())
        .chain_update(span.scalar_start.to_le_bytes())
        .chain_update(span.utf16_start.to_le_bytes())
        .chain_update([level.unwrap_or(0)])
        .chain_update(kind.as_bytes())
        .chain_update([0])
        .chain_update(span.heading_markdown.as_deref().unwrap_or("").as_bytes())
        .finalize()
        .into()
}

fn block_label(kind: DisplayBlockKind) -> (&'static str, Option<u8>) {
    match kind {
        DisplayBlockKind::Paragraph => ("paragraph", None),
        DisplayBlockKind::Heading(level) => ("heading", Some(level)),
        DisplayBlockKind::BlockQuote => ("block_quote", None),
        DisplayBlockKind::Code => ("code", None),
        DisplayBlockKind::List => ("list", None),
        DisplayBlockKind::Rule => ("rule", None),
        DisplayBlockKind::Table => ("table", None),
    }
}

fn parse_kind(kind: &str, level: Option<u8>) -> Result<DisplayBlockKind, StoreError> {
    Ok(match (kind, level) {
        ("paragraph", None) => DisplayBlockKind::Paragraph,
        ("heading", Some(level @ 1..=6)) => DisplayBlockKind::Heading(level),
        ("block_quote", None) => DisplayBlockKind::BlockQuote,
        ("code", None) => DisplayBlockKind::Code,
        ("list", None) => DisplayBlockKind::List,
        ("rule", None) => DisplayBlockKind::Rule,
        ("table", None) => DisplayBlockKind::Table,
        _ => return Err(AppError::Internal.into()),
    })
}
