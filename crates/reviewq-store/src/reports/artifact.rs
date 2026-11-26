//! The immutable text of a report version, written once with its hash and statistics.

use reviewq_app::access::AppError;
use reviewq_app::reports::{ArtifactStats, CHUNK_BYTES};
use reviewq_domain::identity::ReportHash;
use rusqlite::{Connection, params};

use crate::StoreError;

/// Writes `text` as artifact `artifact`: one row with its hash and statistics, then its
/// chunks of at most `CHUNK_BYTES` in order, each ending on a character boundary and carrying
/// its byte and scalar starts. Call it inside the transaction that references the artifact.
pub(crate) fn write(
    db: &Connection,
    artifact: &str,
    text: &str,
    hash: &ReportHash,
    stats: &ArtifactStats,
) -> Result<(), StoreError> {
    db.prepare_cached(
        "INSERT INTO artifacts(artifact_id, sha256, byte_length, total_code_points, line_count,
           max_line_bytes) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![
        artifact,
        hash.as_str(),
        db_int(stats.byte_length)?,
        db_int(stats.total_code_points)?,
        db_int(stats.line_count)?,
        db_int(stats.max_line_bytes)?
    ])?;
    let mut insert = db.prepare_cached(
        "INSERT INTO artifact_chunks(artifact_id, ordinal, byte_start, scalar_start, raw)
         VALUES(?1, ?2, ?3, ?4, ?5)",
    )?;
    let (mut bytes, mut scalars) = (0_u64, 0_u64);
    for (ordinal, chunk) in chunks(text).enumerate() {
        insert.execute(params![
            artifact,
            db_int(ordinal as u64)?,
            db_int(bytes)?,
            db_int(scalars)?,
            chunk.as_bytes()
        ])?;
        bytes += chunk.len() as u64;
        scalars += chunk.chars().count() as u64;
    }
    Ok(())
}

/// The text in chunks of at most `CHUNK_BYTES`, each ending on a character boundary.
fn chunks(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = rest.len().min(CHUNK_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, tail) = rest.split_at(end);
        rest = tail;
        Some(chunk)
    })
}

fn db_int(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| AppError::Internal.into())
}
