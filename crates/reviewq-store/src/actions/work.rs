use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::actions::{
    ActionWork, PublicationEvidence, PublicationRetry, PublicationWorkerStore,
};
use reviewq_app::events::ResourceKey;
use rusqlite::{Connection, params};

use super::commands::{record_evidence, work_for_lead};
use crate::events::now_ms;
use crate::queue::{lead_for_run, parse, text};
use crate::{SqliteStore, StoreError};

impl PublicationWorkerStore for SqliteStore {
    fn due_publications(&self, limit: u16) -> AppFuture<'_, Vec<ActionWork>> {
        Box::pin(async move {
            if !(1..=16).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |db| due(db, limit))
                .await
                .map_err(StoreError::app)
        })
    }

    fn defer_publication(&self, work: ActionWork, reason: &'static str) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                let when=now_ms()?;
                let (previous,uncertainty):(Option<String>,u8)=tx.sql.query_row(
                    "SELECT reason,uncertainty_checks FROM actions WHERE action_id=?1 AND revision=?2
                     AND authority_verified=1 AND state IN ('working','checking','retry_wait','needs_input')",
                    params![work.action.id.as_str(),work.action.revision.as_sqlite_integer()],
                    |row|Ok((row.get(0)?,row.get(1)?)),
                ).map_err(|_|AppError::StaleRevision)?;
                let delay=if reason=="publication_native_work_active" {10_000}
                    else {10_000_i64<<uncertainty.min(3)};
                tx.sql.execute(
                    "UPDATE actions SET due_at_ms=?2,reason=?3,uncertainty_checks=min(uncertainty_checks+1,4)
                     WHERE action_id=?1",
                    params![work.action.id.as_str(),when.saturating_add(delay),reason],
                )?;
                if previous.as_deref()!=Some(reason) {
                    tx.emit(&[ResourceKey::Action(Some(work.target.run_id().clone()))])?;
                }
                Ok(())
            }).await.map_err(StoreError::app)
        })
    }

    fn reserve_publication(
        &self,
        work: ActionWork,
        evidence: PublicationEvidence,
        retry: PublicationRetry,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                record_evidence(tx, work, evidence, false, Some(retry))?;
                Ok(())
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn due(db: &Connection, limit: u16) -> Result<Vec<ActionWork>, StoreError> {
    let mut statement=db.prepare_cached(
        "SELECT a.run_id FROM actions a JOIN action_heads h USING(run_id,action_id)
         JOIN runs q ON q.run_id=a.run_id JOIN reviews r ON r.review_id=q.review_id
         JOIN configured_projects p ON p.origin=r.origin AND p.project_id=r.project_id
         WHERE a.authority_verified=1 AND a.state IN ('working','checking','retry_wait','needs_input')
           AND a.due_at_ms<=?1
         ORDER BY a.due_at_ms,a.action_id LIMIT ?2",
    )?;
    let mut rows = statement.query(params![now_ms()?, limit])?;
    let mut output = Vec::new();
    while let Some(row) = rows.next()? {
        let run = parse(text(row, 0)?)?;
        let Some(lead) = lead_for_run(db, &run)? else {
            continue;
        };
        if let Some(item) = work_for_lead(db, lead)? {
            output.push(item);
        }
    }
    Ok(output)
}
