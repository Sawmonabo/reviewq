//! The immutable text of a report version, appended chunk by chunk and sealed once.

use reviewq_app::access::AppError;
use reviewq_app::reports::{ArtifactChunk, ArtifactStats, CHUNK_BYTES};
use reviewq_domain::identity::ReportHash;
use rusqlite::{Connection, params};

use crate::StoreError;

/// Appends the next chunk of an unsealed artifact. A chunk that is empty, larger than
/// `CHUNK_BYTES`, not UTF-8, out of order or off the artifact's running offsets is a conflict
/// and writes nothing.
#[expect(
    dead_code,
    reason = "K2 appends the composed text of a published version"
)]
pub(crate) fn append(
    db: &Connection,
    artifact: &str,
    chunk: &ArtifactChunk,
) -> Result<(), StoreError> {
    if chunk.bytes.is_empty() || chunk.bytes.len() > CHUNK_BYTES {
        return Err(AppError::InvalidRequest.into());
    }
    let scalars = std::str::from_utf8(&chunk.bytes)
        .map_err(|_| AppError::InvalidRequest)?
        .chars()
        .count() as i64;
    let ordinal = db_int(chunk.ordinal)?;
    let start = db_int(chunk.byte_start)?;
    let scalar_start = db_int(chunk.scalar_start)?;
    let valid = db.prepare_cached(
        "UPDATE artifacts SET byte_length=byte_length+?4, total_code_points=total_code_points+?5
         WHERE artifact_id=?1 AND sealed=0 AND byte_length=?2 AND total_code_points=?3
           AND ?6=(SELECT coalesce(max(ordinal), -1)+1 FROM artifact_chunks WHERE artifact_id=?1)",
    )?
    .execute(params![artifact, start, scalar_start, chunk.bytes.len() as i64, scalars, ordinal])?;
    if valid != 1 {
        return Err(AppError::Conflict.into());
    }
    db.prepare_cached(
        "INSERT INTO artifact_chunks(artifact_id, ordinal, byte_start, scalar_start, raw)
         VALUES(?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![artifact, ordinal, start, scalar_start, chunk.bytes])?;
    Ok(())
}

/// Seals an artifact whose appended length matches `stats`, recording its hash and line
/// statistics. Any mismatch, or an artifact already sealed, is a conflict.
#[expect(
    dead_code,
    reason = "K2 seals the composed text of a published version"
)]
pub(crate) fn seal(
    db: &Connection,
    artifact: &str,
    hash: &ReportHash,
    stats: &ArtifactStats,
) -> Result<(), StoreError> {
    let changed = db.execute(
        "UPDATE artifacts SET sha256=?2, line_count=?3, max_line_bytes=?4, sealed=1
         WHERE artifact_id=?1 AND byte_length=?5 AND total_code_points=?6 AND sealed=0",
        params![
            artifact,
            hash.as_str(),
            db_int(stats.line_count)?,
            db_int(stats.max_line_bytes)?,
            db_int(stats.byte_length)?,
            db_int(stats.total_code_points)?
        ],
    )?;
    if changed != 1 {
        return Err(AppError::Conflict.into());
    }
    Ok(())
}

fn db_int(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| AppError::Internal.into())
}
