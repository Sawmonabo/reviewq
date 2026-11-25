use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::reports::{
    Descriptor, DispositionEdit, DispositionReceipt, Publication, ReportMutationStore, ViewReceipt,
    ViewedReport,
};
use reviewq_domain::identity::{ReceiptId, Revision, RunId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use super::{authorize_run, optional};
use crate::connection::WriteTx;
use crate::decisions::{record_receipt, verify_replay};
use crate::events::{now_ms, snapshot};
use crate::queue::{parse, text};
use crate::workspace::{actor_key, hash_field};
use crate::{SqliteStore, StoreError};

const DISPOSITION: &str = "report_disposition";
const VIEWED: &str = "report_viewed";

impl ReportMutationStore for SqliteStore {
    fn set_disposition(
        &self,
        actor: Actor,
        edit: DispositionEdit,
    ) -> AppFuture<'_, Snapshot<DispositionReceipt>> {
        Box::pin(async move {
            if edit.finding_id.is_empty()
                || edit.finding_id.len() > 64
                || edit.finding_id.chars().any(char::is_control)
                || edit.reason.trim().is_empty()
                || edit.reason.len() > 384
                || (edit.blocking.is_none() && edit.publication.is_none())
            {
                return Err(AppError::InvalidRequest);
            }
            self.write(move |tx| disposition(tx, actor, edit))
                .await
                .map_err(StoreError::app)
        })
    }

    fn mark_report_viewed(
        &self,
        actor: Actor,
        view: ViewedReport,
    ) -> AppFuture<'_, Snapshot<ViewReceipt>> {
        Box::pin(async move {
            self.write(move |tx| viewed(tx, actor, view))
                .await
                .map_err(StoreError::app)
        })
    }
}

fn authorize(db: &Connection, actor: &Actor, run: &RunId) -> Result<(), StoreError> {
    if matches!(actor.scope(), ActorScope::NativeSession(_)) {
        return Err(AppError::Forbidden.into());
    }
    authorize_run(db, actor, run)
}

fn disposition(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    edit: DispositionEdit,
) -> Result<Snapshot<DispositionReceipt>, StoreError> {
    let db = &tx.sql;
    authorize(db, &actor, &edit.run)?;
    let digest = disposition_digest(&edit);
    let key = actor_key(&actor);
    if verify_replay(db, &key, DISPOSITION, &edit.submission, &digest)? {
        let revision = db.query_row(
            "SELECT disposition_version FROM report_mutation_receipts
             WHERE actor_key=?1 AND operation=?2 AND submission_id=?3",
            params![key.as_slice(), DISPOSITION, edit.submission.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        return report_disposition(db, &edit.run, edit.receipt, &edit.finding_id, revision);
    }
    let head: Option<(String, i64)> = db
        .query_row(
            "SELECT receipt_id, disposition_version FROM report_heads WHERE run_id=?1",
            [edit.run.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((receipt, revision)) = head else {
        return Err(AppError::NotFound.into());
    };
    if receipt != edit.receipt.as_str() || revision != edit.expected_revision.as_sqlite_integer() {
        return Err(AppError::StaleRevision.into());
    }
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM report_findings WHERE run_id=?1 AND receipt_id=?2 AND finding_id=?3)",
        params![edit.run.as_str(), edit.receipt.as_str(), edit.finding_id], |row| row.get(0),
    )?;
    if !exists {
        return Err(AppError::NotFound.into());
    }
    let previous: Option<(Option<bool>, Option<String>)> = db
        .query_row(
            "SELECT blocking, publication FROM finding_dispositions
         WHERE run_id=?1 AND finding_id=?2 AND disposition_version<=?3
         ORDER BY disposition_version DESC LIMIT 1",
            params![edit.run.as_str(), edit.finding_id, revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (prior_blocking, prior_publication) = previous.unwrap_or_default();
    let next = revision.checked_add(1).ok_or(AppError::Internal)?;
    db.execute(
        "INSERT INTO finding_dispositions(run_id, disposition_version, receipt_id, finding_id,
             blocking, publication, reason, removed) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
        params![
            edit.run.as_str(),
            next,
            edit.receipt.as_str(),
            edit.finding_id,
            edit.blocking.or(prior_blocking),
            edit.publication
                .map(|p| p.to_string())
                .or(prior_publication),
            edit.reason
        ],
    )?;
    db.execute(
        "UPDATE report_heads SET disposition_version=?2 WHERE run_id=?1",
        params![edit.run.as_str(), next],
    )?;
    record_receipt(db, &actor, DISPOSITION, &edit.submission, &digest)?;
    db.execute(
        "INSERT INTO report_mutation_receipts(actor_key, operation, submission_id, run_id, receipt_id,
             disposition_version, finding_id) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![key.as_slice(), DISPOSITION, edit.submission.as_str(), edit.run.as_str(),
            edit.receipt.as_str(), next, edit.finding_id],
    )?;
    tx.emit(&[ResourceKey::Report(Some(edit.run.clone()))])?;
    report_disposition(&tx.sql, &edit.run, edit.receipt, &edit.finding_id, next)
}

fn disposition_digest(edit: &DispositionEdit) -> [u8; 32] {
    let mut digest = Sha256::new();
    for field in [
        edit.run.as_str(),
        edit.receipt.as_str(),
        &edit.finding_id,
        &edit.reason,
    ] {
        hash_field(&mut digest, field.as_bytes());
    }
    digest.update(edit.expected_revision.get().to_be_bytes());
    digest.update([match edit.blocking {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }]);
    if let Some(publication) = edit.publication {
        hash_field(&mut digest, publication.to_string().as_bytes());
    }
    digest.finalize().into()
}

fn report_disposition(
    db: &Connection,
    run: &RunId,
    receipt: ReceiptId,
    id: &str,
    revision: i64,
) -> Result<Snapshot<DispositionReceipt>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT blocking, publication, reason FROM finding_dispositions
         WHERE run_id=?1 AND disposition_version=?2 AND finding_id=?3",
    )?;
    let mut rows = statement.query(params![run.as_str(), revision, id])?;
    let row = rows.next()?.ok_or(AppError::Internal)?;
    snapshot(
        db,
        DispositionReceipt {
            receipt_id: receipt,
            disposition_version: Revision::try_from(revision).map_err(|_| AppError::Internal)?,
            finding: Descriptor::Disposition {
                id: id.to_owned(),
                blocking: row.get(0)?,
                publication: optional::<Publication>(row, 1)?,
                reason: row.get(2)?,
            },
        },
    )
}

fn viewed(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    view: ViewedReport,
) -> Result<Snapshot<ViewReceipt>, StoreError> {
    let db = &tx.sql;
    authorize(db, &actor, &view.run)?;
    let mut hash = Sha256::new();
    for field in [
        view.run.as_str(),
        view.receipt.as_str(),
        view.rendered_batch_id.as_deref().unwrap_or(""),
    ] {
        hash_field(&mut hash, field.as_bytes());
    }
    let digest = hash.finalize().into();
    let key = actor_key(&actor);
    if verify_replay(db, &key, VIEWED, &view.submission, &digest)? {
        return replay_viewed(db, &key, &view);
    }
    super::version(db, &view.run, &view.receipt)?;
    let previous = db.query_row(
        "SELECT v.receipt_id FROM report_reads m JOIN report_versions v USING(run_id, receipt_id)
         WHERE m.viewer_key=?1 AND m.run_id=?2 ORDER BY v.published_at_ms DESC, v.receipt_id DESC LIMIT 1",
        params![key.as_slice(), view.run.as_str()], |row| row.get::<_, String>(0),
    ).optional()?;
    let time = now_ms()?;
    let mut keys = Vec::new();
    if let Some(batch) = &view.rendered_batch_id {
        let review = db.query_row(
            "SELECT review_id FROM followup_results WHERE run_id=?1 AND receipt_id=?2 AND batch_id=?3",
            params![view.run.as_str(), view.receipt.as_str(), batch], |row| row.get::<_, String>(0),
        ).optional()?.ok_or(AppError::NotFound)?;
        let changed = db.execute(
            "INSERT INTO followup_result_reads(viewer_key, review_id, batch_id, viewed_at_ms)
             VALUES(?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
            params![key.as_slice(), review, batch, time],
        )?;
        if changed != 0 {
            keys.push(ResourceKey::Followup(Some(parse(&review)?)));
        }
    }
    let changed = db.execute(
        "INSERT INTO report_reads(viewer_key, run_id, receipt_id, viewed_at_ms)
         VALUES(?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
        params![
            key.as_slice(),
            view.run.as_str(),
            view.receipt.as_str(),
            time
        ],
    )?;
    if changed != 0 {
        keys.push(ResourceKey::Report(Some(view.run.clone())));
    }
    record_receipt(db, &actor, VIEWED, &view.submission, &digest)?;
    db.execute(
        "INSERT INTO report_mutation_receipts(actor_key, operation, submission_id, run_id, receipt_id,
             previous_receipt_id, rendered_batch_id) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![key.as_slice(), VIEWED, view.submission.as_str(), view.run.as_str(),
            view.receipt.as_str(), previous, view.rendered_batch_id],
    )?;
    if !keys.is_empty() {
        tx.emit(&keys)?;
    }
    snapshot(
        &tx.sql,
        ViewReceipt {
            receipt_id: view.receipt,
            previous_receipt_id: previous.as_deref().map(parse).transpose()?,
            rendered_batch_id: view.rendered_batch_id,
        },
    )
}

fn replay_viewed(
    db: &Connection,
    key: &[u8; 32],
    view: &ViewedReport,
) -> Result<Snapshot<ViewReceipt>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT receipt_id, previous_receipt_id, rendered_batch_id FROM report_mutation_receipts
         WHERE actor_key=?1 AND operation=?2 AND submission_id=?3",
    )?;
    let mut rows = statement.query(params![key.as_slice(), VIEWED, view.submission.as_str()])?;
    let row: &Row<'_> = rows.next()?.ok_or(AppError::Internal)?;
    snapshot(
        db,
        ViewReceipt {
            receipt_id: parse(text(row, 0)?)?,
            previous_receipt_id: optional(row, 1)?,
            rendered_batch_id: row.get(2)?,
        },
    )
}
