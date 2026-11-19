use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;
use crate::events::now_ms;
use crate::workspace::queue_revision;

const SESSION_FRESH_MS: i64 = 30_000;
const COLUMNS: &str = "review_id,run_id,focus_run,focus_session,own,personal_approval,our_comments,
  unread_replies,has_questions,stage_reason,queue_state,full_mr_reviewed_sha,coverage,last_activity,hide_reviewed";

/// Shares compact classified rows across reads of the same committed generation.
/// TEMP storage belongs to the reader connection. Its main database handle is read-only.
pub(super) fn projection(db: &Connection) -> Result<i64, StoreError> {
    let revision = queue_revision(db)?.as_sqlite_integer();
    let now = now_ms()?;
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM temp.sqlite_schema WHERE name='queue_projection_stamp')",
        [],
        |row| row.get(0),
    )?;
    if exists {
        let cached=db.prepare_cached(
            "SELECT built_at_ms FROM queue_projection_stamp WHERE revision=?1 AND (expires_at_ms IS NULL OR expires_at_ms>?2)"
        )?.query_row(params![revision,now],|row|row.get::<_,i64>(0)).optional()?;
        if let Some(cached) = cached {
            return Ok(cached);
        }
    }
    db.pragma_update(None, "query_only", false)?;
    let result = rebuild(db, revision, now, exists);
    db.pragma_update(None, "query_only", true)?;
    result.map(|()| now)
}

fn rebuild(db: &Connection, revision: i64, now: i64, exists: bool) -> Result<(), StoreError> {
    if !exists {
        db.execute_batch(&format!(
            "CREATE TEMP TABLE queue_projection AS SELECT {COLUMNS} FROM review_activity WHERE 0;
             CREATE UNIQUE INDEX temp.queue_projection_review ON queue_projection(review_id);
             CREATE INDEX temp.queue_projection_stage ON queue_projection(stage_reason,last_activity DESC,review_id);
             CREATE TEMP TABLE queue_projection_stamp(revision INTEGER NOT NULL,built_at_ms INTEGER NOT NULL,expires_at_ms INTEGER);"
        ))?;
    } else {
        db.execute_batch("DELETE FROM queue_projection; DELETE FROM queue_projection_stamp;")?;
    }
    db.prepare_cached(&format!(
        "INSERT INTO queue_projection SELECT {COLUMNS} FROM review_activity"
    ))?
    .execute([])?;
    let expires: Option<i64> = db.query_row(
        "SELECT min(seen_at_ms+?1) FROM claude_companions WHERE seen_at_ms>?2",
        params![SESSION_FRESH_MS, now.saturating_sub(SESSION_FRESH_MS)],
        |row| row.get(0),
    )?;
    db.prepare_cached("INSERT INTO queue_projection_stamp VALUES (?1,?2,?3)")?
        .execute(params![revision, now, expires])?;
    Ok(())
}
