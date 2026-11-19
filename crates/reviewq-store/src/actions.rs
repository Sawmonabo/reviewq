//! Scoped read of the current action and its bounded individual targets.

use reviewq_app::access::{Actor, AppError, AppFuture};
mod commands;
use reviewq_app::actions::{Action, ActionStore, Target};
use reviewq_app::events::Snapshot;
use reviewq_domain::identity::{ActionId, Revision, RunId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::events::snapshot;
use crate::queue::{parse, text};
use crate::reports::authorize_run;
use crate::{SqliteStore, StoreError};

impl ActionStore for SqliteStore {
    fn read_action(&self, actor: Actor, run: RunId) -> AppFuture<'_, Snapshot<Option<Action>>> {
        Box::pin(async move {
            self.read(move |db| {
                authorize_run(db, &actor, &run)?;
                snapshot(db, current(db, &run)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn current(db: &Connection, run: &RunId) -> Result<Option<Action>, StoreError> {
    let id = db
        .query_row(
            "SELECT action_id FROM action_heads WHERE run_id=?1",
            [run.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(id) = id else { return Ok(None) };
    by_id(db, run, &id)
}

fn by_id(db: &Connection, run: &RunId, action: &str) -> Result<Option<Action>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT a.action_id, a.request_entry_id, a.native_turn_id, a.native_call_id, a.reviewed_sha,
            a.authority_verified, a.revision, a.intent, a.state, a.approval_state, a.approval_observed_at_ms,
            a.attempts, a.pending_submission_id, a.last_executed_submission_id, a.last_executed_turn_id,
            a.due_at_ms, a.reason, a.checked_at_ms
         FROM actions a WHERE a.run_id=?1 AND a.action_id=?2",
    )?;
    let mut rows = statement.query(params![run.as_str(), action])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let id = parse(text(row, 0)?)?;
    let action = Action {
        targets: targets(db, &id)?,
        id,
        run: run.clone(),
        request_entry: optional(row, 1)?,
        turn: optional(row, 2)?,
        call_id: row.get(3)?,
        reviewed_sha: optional(row, 4)?,
        authority_verified: row.get(5)?,
        revision: Revision::try_from(row.get::<_, i64>(6)?).map_err(|_| AppError::Internal)?,
        intent: parse(text(row, 7)?)?,
        state: parse(text(row, 8)?)?,
        approval: parse(text(row, 9)?)?,
        approval_observed_at_ms: row.get(10)?,
        attempts: row.get(11)?,
        pending_submission: optional(row, 12)?,
        last_executed_submission: optional(row, 13)?,
        last_executed_turn: optional(row, 14)?,
        next_check_at_ms: row.get(15)?,
        reason: row.get(16)?,
        checked_at_ms: row.get(17)?,
    };
    Ok(Some(action))
}

fn targets(db: &Connection, action: &ActionId) -> Result<Vec<Target>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT body_sha256, discussion_id, evidence_state, note_id, observed_at_ms
         FROM action_targets WHERE action_id=?1 ORDER BY ordinal",
    )?;
    let mut rows = statement.query(params![action.as_str()])?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next()? {
        targets.push(Target {
            body_sha256: parse(text(row, 0)?)?,
            discussion_id: row.get(1)?,
            evidence: parse(text(row, 2)?)?,
            note_id: row.get(3)?,
            observed_at_ms: row.get(4)?,
        });
    }
    Ok(targets)
}

fn optional<T: std::str::FromStr>(row: &Row<'_>, column: usize) -> Result<Option<T>, StoreError> {
    row.get::<_, Option<String>>(column)?
        .as_deref()
        .map(parse)
        .transpose()
}
