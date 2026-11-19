//! Native presentation facts keyed by an already verified session.

use reviewq_app::hosts::NativeSessionMetadata;
use rusqlite::{Connection, Row, params};

use crate::StoreError;

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

pub(crate) fn record(
    db: &Connection,
    session: &str,
    value: &NativeSessionMetadata,
) -> Result<bool, StoreError> {
    let observed = (
        value.name.as_deref(),
        value.status.as_deref(),
        value.started_at_ms,
        value.model.as_deref(),
        value.effort.as_deref(),
    );
    let changed = db.execute(
        "UPDATE sessions SET native_name=coalesce(?2,native_name), native_status=coalesce(?3,native_status),
            native_started_at_ms=coalesce(?4,native_started_at_ms), native_model=coalesce(?5,native_model),
            native_effort=coalesce(?6,native_effort)
         WHERE session_key=?1 AND (
            native_name IS DISTINCT FROM coalesce(?2,native_name)
            OR native_status IS DISTINCT FROM coalesce(?3,native_status)
            OR native_started_at_ms IS DISTINCT FROM coalesce(?4,native_started_at_ms)
            OR native_model IS DISTINCT FROM coalesce(?5,native_model)
            OR native_effort IS DISTINCT FROM coalesce(?6,native_effort)
         )",
        params![session, observed.0, observed.1, observed.2, observed.3, observed.4],
    )?;
    Ok(changed != 0)
}
