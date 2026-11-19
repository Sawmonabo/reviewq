use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::sources::{
    HistoryCandidate, MAX_PUBLICATION_REVIEWS, ProjectHistoryRow, ProjectHistoryStore,
    RefreshAttempt,
};
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::{MergeRequestIid, TicketKey};
use rusqlite::{Connection, OptionalExtension, params};

use crate::queue::{parse, text};
use crate::workspace::settings_revision;
use crate::{SqliteStore, StoreError};

const MAX_SCAN_BYTES: i64 = 64 * 1024 * 1024;
const MAX_PAGE_SIZE: usize = 100;

impl ProjectHistoryStore for SqliteStore {
    fn begin_project_history(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
    ) -> AppFuture<'_, Option<i64>> {
        Box::pin(async move {
            self.write(move |tx| {
                check(&tx.sql, attempt, &project)?;
                tx.sql.execute("DELETE FROM source_history_staging", [])?;
                tx.sql.execute(
                    "INSERT INTO source_history_scan VALUES(1,?1,?2,?3,0)
                     ON CONFLICT(singleton) DO UPDATE SET attempt=excluded.attempt,
                         origin=excluded.origin,project_id=excluded.project_id,bytes=0",
                    params![
                        attempt.id.as_sqlite_integer(),
                        project.origin.as_str(),
                        project.project_id.as_sqlite_integer()
                    ],
                )?;
                watermark(&tx.sql, attempt, &project)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn stage_project_history(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
        rows: Vec<ProjectHistoryRow>,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            if rows.len() > MAX_PAGE_SIZE {
                return Err(AppError::PayloadTooLarge);
            }
            self.write(move |tx| {
                check_scan(&tx.sql, attempt, &project)?;
                stage(&tx.sql, rows)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn finish_project_history(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
        watermark: i64,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                check_scan(&tx.sql, attempt, &project)?;
                publish(&tx.sql, attempt, &project, watermark)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn invalidate_project_history(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                check_scan(&tx.sql, attempt, &project)?;
                tx.sql.execute(
                    "UPDATE source_history_projects SET valid=0 WHERE (origin,project_id,settings_revision)=(?1,?2,?3)",
                    params![project.origin.as_str(),project.project_id.as_sqlite_integer(),attempt.settings_revision.as_sqlite_integer()]
                )?;
                tx.sql.execute("DELETE FROM source_history_staging", [])?;
                tx.sql.execute("DELETE FROM source_history_scan", [])?;
                Ok(())
            }).await.map_err(StoreError::app)
        })
    }

    fn project_history_candidates(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
        keys: Vec<TicketKey>,
    ) -> AppFuture<'_, Vec<HistoryCandidate>> {
        Box::pin(async move {
            if keys.len() > 8192 {
                return Err(AppError::PayloadTooLarge);
            }
            self.read(move |db| {
                check(db, attempt, &project)?;
                if watermark(db, attempt, &project)?.is_none() {
                    return Err(AppError::StaleRevision.into());
                }
                candidates(db, project, keys)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn stage(db: &Connection, rows: Vec<ProjectHistoryRow>) -> Result<(), StoreError> {
    let mut insert = db.prepare_cached(
        "INSERT INTO source_history_staging VALUES (?1,?2,?3,?4,?5,?6)
                                       ON CONFLICT(iid) DO NOTHING",
    )?;
    let mut exact = db.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM source_history_staging
                       WHERE (iid,title,branch,updated_at_ms,closed,keys)=(?1,?2,?3,?4,?5,?6))",
    )?;
    let mut charge = 0;
    for row in rows {
        if row.title.len() > 4096 || row.branch.len() > 1024 || row.keys.len() > 16 {
            return Err(AppError::PayloadTooLarge.into());
        }
        let keys =
            serde_json::to_string(&row.keys.iter().map(TicketKey::as_str).collect::<Vec<_>>())
                .map_err(|_| AppError::Internal)?;
        let args = params![
            row.iid.as_sqlite_integer(),
            row.title,
            row.branch,
            row.updated_at_ms,
            row.closed,
            keys
        ];
        if insert.execute(args)? == 0 {
            if !exact.query_row(args, |row| row.get::<_, bool>(0))? {
                return Err(AppError::Conflict.into());
            }
        } else {
            charge += row.title.len() + row.branch.len() + keys.len() + 64;
        }
    }
    let charged = db.execute(
        "UPDATE source_history_scan SET bytes=bytes+?1 WHERE bytes+?1<=?2",
        params![charge as i64, MAX_SCAN_BYTES],
    )?;
    if charged != 1 {
        return Err(AppError::PayloadTooLarge.into());
    }
    Ok(())
}

fn publish(
    db: &Connection,
    attempt: RefreshAttempt,
    project: &ConfiguredProject,
    next: i64,
) -> Result<(), StoreError> {
    let prior = watermark(db, attempt, project)?;
    let args = params![
        project.origin.as_str(),
        project.project_id.as_sqlite_integer()
    ];
    if prior.is_none() {
        db.execute(
            "DELETE FROM source_history_projects WHERE (origin,project_id)=(?1,?2)",
            args,
        )?;
    }
    db.execute(
        "INSERT INTO source_history_projects VALUES(?1,?2,?3,?4,1)
         ON CONFLICT(origin,project_id) DO UPDATE SET watermark_ms=excluded.watermark_ms
         WHERE watermark_ms<excluded.watermark_ms",
        params![
            project.origin.as_str(),
            project.project_id.as_sqlite_integer(),
            attempt.settings_revision.as_sqlite_integer(),
            next
        ],
    )?;
    // Elide overlapping rows whose canonical facts and closed state are unchanged.
    db.execute(
        "DELETE FROM source_history_staging AS s WHERE (s.closed=0 AND NOT EXISTS(
             SELECT 1 FROM source_history_rows r WHERE (r.origin,r.project_id,r.iid)=(?1,?2,s.iid)))
         OR EXISTS(SELECT 1 FROM source_history_rows r WHERE (r.origin,r.project_id,r.iid)=(?1,?2,s.iid)
            AND (r.updated_at_ms>s.updated_at_ms OR
                 (s.closed=1 AND (r.title,r.branch,r.updated_at_ms)=(s.title,s.branch,s.updated_at_ms))))",
        args
    )?;
    db.execute(
        "DELETE FROM source_history_rows AS r
         WHERE (r.origin,r.project_id)=(?1,?2) AND r.iid IN (SELECT iid FROM source_history_staging)",
        args
    )?;
    db.execute(
        "INSERT INTO source_history_rows
         SELECT ?1,?2,s.iid,s.title,s.branch,s.updated_at_ms FROM source_history_staging s WHERE s.closed=1",
        args
    )?;
    db.execute(
        "INSERT INTO source_history_keys
         SELECT ?1,?2,s.iid,k.value FROM source_history_staging s JOIN json_each(s.keys) k WHERE s.closed=1",
        args
    )?;
    db.execute("DELETE FROM source_history_staging", [])?;
    db.execute("DELETE FROM source_history_scan", [])?;
    Ok(())
}

fn candidates(
    db: &Connection,
    project: ConfiguredProject,
    keys: Vec<TicketKey>,
) -> Result<Vec<HistoryCandidate>, StoreError> {
    let wanted = serde_json::to_string(&keys.iter().map(TicketKey::as_str).collect::<Vec<_>>())
        .map_err(|_| AppError::Internal)?;
    let mut query = db.prepare_cached(
        "SELECT m.iid,json_group_array(m.ticket_key)
         FROM json_each(?3) q JOIN source_history_keys m ON m.ticket_key=q.value
         WHERE (m.origin,m.project_id)=(?1,?2)
         GROUP BY m.iid ORDER BY m.iid LIMIT ?4",
    )?;
    let mut rows = query.query(params![
        project.origin.as_str(),
        project.project_id.as_sqlite_integer(),
        wanted,
        MAX_PUBLICATION_REVIEWS as i64 + 1
    ])?;
    let mut matching = Vec::new();
    while let Some(row) = rows.next()? {
        if matching.len() == MAX_PUBLICATION_REVIEWS {
            return Err(AppError::PayloadTooLarge.into());
        }
        let keys: Vec<String> =
            serde_json::from_str(text(row, 1)?).map_err(|_| AppError::Internal)?;
        matching.push(HistoryCandidate {
            iid: MergeRequestIid::try_from(row.get::<_, i64>(0)?)
                .map_err(|_| AppError::Internal)?,
            keys: keys
                .iter()
                .map(|key| parse(key))
                .collect::<Result<Vec<_>, _>>()?,
        });
    }
    Ok(matching)
}

fn watermark(
    db: &Connection,
    attempt: RefreshAttempt,
    project: &ConfiguredProject,
) -> Result<Option<i64>, StoreError> {
    Ok(db
        .query_row(
            "SELECT watermark_ms FROM source_history_projects
         WHERE (origin,project_id,settings_revision,valid)=(?1,?2,?3,1)",
            params![
                project.origin.as_str(),
                project.project_id.as_sqlite_integer(),
                attempt.settings_revision.as_sqlite_integer()
            ],
            |row| row.get(0),
        )
        .optional()?)
}

fn check(
    db: &Connection,
    attempt: RefreshAttempt,
    project: &ConfiguredProject,
) -> Result<(), StoreError> {
    let active = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_refresh f JOIN configured_projects p
         WHERE (f.attempt,f.settings_revision,f.active,p.origin,p.project_id)=(?1,?2,1,?3,?4))",
        params![
            attempt.id.as_sqlite_integer(),
            attempt.settings_revision.as_sqlite_integer(),
            project.origin.as_str(),
            project.project_id.as_sqlite_integer()
        ],
        |row| row.get::<_, bool>(0),
    )?;
    if !active || settings_revision(db)? != attempt.settings_revision {
        return Err(AppError::StaleRevision.into());
    }
    Ok(())
}

fn check_scan(
    db: &Connection,
    attempt: RefreshAttempt,
    project: &ConfiguredProject,
) -> Result<(), StoreError> {
    check(db, attempt, project)?;
    let same = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_history_scan WHERE (attempt,origin,project_id)=(?1,?2,?3))",
        params![attempt.id.as_sqlite_integer(),project.origin.as_str(),project.project_id.as_sqlite_integer()],
        |row| row.get::<_,bool>(0)
    )?;
    if !same {
        return Err(AppError::StaleRevision.into());
    }
    Ok(())
}
