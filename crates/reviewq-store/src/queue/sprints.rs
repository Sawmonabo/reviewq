use std::collections::BTreeSet;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::queue::{LoadedSprint, SprintPair, SprintStore};
use reviewq_domain::identity::Revision;
use rusqlite::{Connection, params};

use crate::events::snapshot;
use crate::workspace::{advance_queue, operator, settings_revision};
use crate::{SqliteStore, StoreError};

impl SprintStore for SqliteStore {
    fn unloaded_sprints(
        &self,
        actor: Actor,
        settings: Revision,
        pairs: Vec<SprintPair>,
    ) -> AppFuture<'_, Snapshot<Vec<SprintPair>>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.read(move |db| snapshot(db, unloaded(db, settings, pairs)?))
                .await
                .map_err(StoreError::app)
        })
    }

    fn publish_sprints(
        &self,
        actor: Actor,
        settings: Revision,
        loaded: Vec<LoadedSprint>,
    ) -> AppFuture<'_, Snapshot<Vec<SprintPair>>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.write(move |tx| {
                let pairs = loaded
                    .iter()
                    .map(|entry| SprintPair {
                        board: entry.sprint.board,
                        sprint: entry.sprint.id,
                    })
                    .collect::<Vec<_>>();
                let missing = unloaded(&tx.sql, settings, pairs)?
                    .into_iter()
                    .collect::<BTreeSet<_>>();
                if !missing.is_empty() {
                    insert(&tx.sql, loaded, missing)?;
                    advance_queue(&tx.sql)?;
                    tx.emit(&[ResourceKey::Queue])?;
                }
                snapshot(&tx.sql, loaded_pairs(&tx.sql)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn insert(
    db: &Connection,
    loaded: Vec<LoadedSprint>,
    missing: BTreeSet<SprintPair>,
) -> Result<(), StoreError> {
    let mut members: usize = db
        .query_row("SELECT count(*) FROM loaded_sprint_issues", [], |row| {
            row.get::<_, i64>(0)
        })?
        .try_into()
        .map_err(|_| AppError::Internal)?;
    let mut add = db.prepare_cached("INSERT INTO loaded_sprints VALUES(?1,?2,?3)")?;
    let mut link = db.prepare_cached("INSERT INTO loaded_sprint_issues VALUES(?1,?2,?3)")?;
    for entry in loaded {
        let pair = SprintPair {
            board: entry.sprint.board,
            sprint: entry.sprint.id,
        };
        if !missing.contains(&pair) {
            continue;
        }
        members = members.saturating_add(entry.tickets.len());
        if members > 16_384 {
            return Err(AppError::PayloadTooLarge.into());
        }
        let (board, id) = numbers(pair)?;
        add.execute(params![board, id, entry.sprint.label])?;
        for ticket in entry.tickets {
            link.execute(params![board, id, ticket.as_str()])?;
        }
    }
    Ok(())
}

fn unloaded(
    db: &Connection,
    settings: Revision,
    pairs: Vec<SprintPair>,
) -> Result<Vec<SprintPair>, StoreError> {
    if settings_revision(db)? != settings {
        return Err(AppError::StaleRevision.into());
    }
    if pairs.len() > 32 {
        return Err(AppError::InvalidRequest.into());
    }
    let mut current = loaded_pairs(db)?.into_iter().collect::<BTreeSet<_>>();
    let mut exists=db.prepare_cached("SELECT EXISTS(SELECT 1 FROM settings_boards WHERE board_id=?1 AND enabled=1 AND kind='scrum')")?;
    let mut missing = Vec::new();
    for pair in pairs {
        let (board, _) = numbers(pair)?;
        if !exists.query_row([board], |row| row.get::<_, bool>(0))? {
            return Err(AppError::NotFound.into());
        }
        if current.insert(pair) {
            missing.push(pair);
        }
    }
    if current.len() > 32 {
        return Err(AppError::Conflict.into());
    }
    Ok(missing)
}

fn loaded_pairs(db: &Connection) -> Result<Vec<SprintPair>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT board_id,sprint_id FROM loaded_sprints ORDER BY board_id,sprint_id",
    )?;
    let mut rows = statement.query([])?;
    let mut pairs = Vec::new();
    while let Some(row) = rows.next()? {
        pairs.push(SprintPair {
            board: row
                .get::<_, i64>(0)?
                .try_into()
                .map_err(|_| AppError::Internal)?,
            sprint: row
                .get::<_, i64>(1)?
                .try_into()
                .map_err(|_| AppError::Internal)?,
        });
    }
    Ok(pairs)
}

fn numbers(pair: SprintPair) -> Result<(i64, i64), AppError> {
    if pair.board == 0 || pair.sprint == 0 {
        return Err(AppError::InvalidRequest);
    }
    Ok((
        pair.board
            .try_into()
            .map_err(|_| AppError::InvalidRequest)?,
        pair.sprint
            .try_into()
            .map_err(|_| AppError::InvalidRequest)?,
    ))
}
