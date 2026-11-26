//! Native presentation facts keyed by an already verified session.

use reviewq_app::hosts::NativeSessionMetadata;
use rusqlite::{Connection, Row, params};

use crate::StoreError;

pub(crate) enum MetadataUpdate {
    Full,
    Partial,
}

pub(crate) fn read(db: &Connection, session: &str) -> Result<NativeSessionMetadata, StoreError> {
    Ok(db.query_row(
        "SELECT s.native_name, coalesce(s.native_status, s.host_activity), s.native_started_at_ms,
                coalesce(s.native_model, c.model), coalesce(s.native_effort, c.effort)
         FROM sessions s LEFT JOIN claude_companions c USING(session_key)
         WHERE s.session_key=?1",
        [session],
        |row| from_row(row, 0),
    )?)
}

pub(crate) fn from_row(row: &Row<'_>, index: usize) -> rusqlite::Result<NativeSessionMetadata> {
    Ok(NativeSessionMetadata {
        name: row.get(index)?,
        status: row.get(index + 1)?,
        started_at_ms: row.get(index + 2)?,
        model: row.get(index + 3)?,
        effort: row.get(index + 4)?,
    })
}

/// Stores the observed facts. A full update replaces name, status and start time, clearing
/// any the session no longer reports; an update that carries no fact at all came without
/// thread data and keeps every stored one. Model and effort keep their stored values.
pub(crate) fn record(
    db: &Connection,
    session: &str,
    value: &NativeSessionMetadata,
    update: MetadataUpdate,
) -> Result<bool, StoreError> {
    let observed = (
        value.name.as_deref(),
        value.status.as_deref(),
        value.started_at_ms,
        value.model.as_deref(),
        value.effort.as_deref(),
    );
    let changed = db.execute(
        "WITH observed AS (
            SELECT CASE WHEN ?7 THEN ?2 ELSE coalesce(?2,native_name) END AS name,
                CASE WHEN ?7 THEN ?3 ELSE coalesce(?3,native_status) END AS status,
                CASE WHEN ?7 THEN ?4 ELSE coalesce(?4,native_started_at_ms) END AS started_at_ms,
                coalesce(?5,native_model) AS model,
                CASE WHEN ?5 IS NOT NULL AND ?5 IS NOT native_model THEN ?6
                    ELSE coalesce(?6,native_effort) END AS effort
            FROM sessions WHERE session_key=?1
         ) UPDATE sessions
            SET native_name=observed.name, native_status=observed.status,
                native_started_at_ms=observed.started_at_ms, native_model=observed.model,
                native_effort=observed.effort
         FROM observed WHERE session_key=?1 AND
            (native_name,native_status,native_started_at_ms,native_model,native_effort)
              IS DISTINCT FROM (observed.name,observed.status,observed.started_at_ms,observed.model,observed.effort)",
        params![
            session,
            observed.0,
            observed.1,
            observed.2,
            observed.3,
            observed.4,
            matches!(update, MetadataUpdate::Full) && *value != NativeSessionMetadata::default()
        ],
    )?;
    Ok(changed != 0)
}
