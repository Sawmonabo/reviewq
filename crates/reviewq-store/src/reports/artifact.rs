//! The immutable text of a report version, appended chunk by chunk and sealed once.

use reviewq_app::access::AppError;
use reviewq_app::reports::{ArtifactStats, CHUNK_BYTES};
use reviewq_domain::identity::ReportHash;
use rusqlite::{Connection, params};

use crate::StoreError;

/// Appends the next chunk of an unsealed artifact at its byte and scalar starts. A chunk that
/// is empty, larger than `CHUNK_BYTES`, out of order or off the artifact's running offsets is a
/// conflict and writes nothing.
pub(crate) fn append(
    db: &Connection,
    artifact: &str,
    ordinal: u64,
    start: (u64, u64),
    text: &str,
) -> Result<(), StoreError> {
    if text.is_empty() || text.len() > CHUNK_BYTES {
        return Err(AppError::InvalidRequest.into());
    }
    let scalars = db_int(text.chars().count() as u64)?;
    let ordinal = db_int(ordinal)?;
    let (byte_start, scalar_start) = (db_int(start.0)?, db_int(start.1)?);
    let valid = db.prepare_cached(
        "UPDATE artifacts SET byte_length=byte_length+?4, total_code_points=total_code_points+?5
         WHERE artifact_id=?1 AND sealed=0 AND byte_length=?2 AND total_code_points=?3
           AND ?6=(SELECT coalesce(max(ordinal), -1)+1 FROM artifact_chunks WHERE artifact_id=?1)",
    )?
    .execute(params![artifact, byte_start, scalar_start, db_int(text.len() as u64)?, scalars, ordinal])?;
    if valid != 1 {
        return Err(AppError::Conflict.into());
    }
    db.prepare_cached(
        "INSERT INTO artifact_chunks(artifact_id, ordinal, byte_start, scalar_start, raw)
         VALUES(?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![
        artifact,
        ordinal,
        byte_start,
        scalar_start,
        text.as_bytes()
    ])?;
    Ok(())
}

/// Seals an artifact whose appended length matches `stats`, recording its hash and line
/// statistics. Any mismatch, or an artifact already sealed, is a conflict.
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
