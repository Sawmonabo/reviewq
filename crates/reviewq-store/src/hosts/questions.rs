use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::ResourceKey;
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{
    QuestionAdmission, QuestionAnchor, QuestionKey, QuestionReceipt, QuestionStatus, QuestionStore,
};
use reviewq_domain::identity::SubmissionId;
use reviewq_domain::session::HostProvider;
use rusqlite::{Connection, Row, params};

use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::history::history_session;
use crate::queue::{parse, text};
use crate::workspace::actor_key;
use crate::{SqliteStore, StoreError};

const FIELDS: &str = "request_id,native_turn_id,native_item_id,source_generation,source_position,
                     blocking,answer_digest,state";

impl QuestionStore for SqliteStore {
    fn question_attempts(
        &self,
        actor: Actor,
        session: HistorySession,
        request: Option<String>,
    ) -> AppFuture<'_, Vec<QuestionReceipt>> {
        Box::pin(async move {
            self.read(move |db| {
                check(db, &actor, &session)?;
                let (address, count) = if request.is_some() {
                    (" AND request_id=?2", 2)
                } else {
                    ("", 1)
                };
                let mut read = db.prepare_cached(&format!(
                    "SELECT {FIELDS} FROM question_attempts WHERE session_key=?1{address}
                     ORDER BY updated_at_ms DESC, request_id LIMIT 8"
                ))?;
                let values = [
                    Some(session.target.session_key().as_str()),
                    request.as_deref(),
                ];
                let mut rows = read.query(rusqlite::params_from_iter(values[..count].iter()))?;
                let mut receipts = Vec::new();
                while let Some(row) = rows.next()? {
                    receipts.push(receipt(row)?);
                }
                Ok(receipts)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn replay_question(
        &self,
        actor: Actor,
        session: HistorySession,
        request: String,
        submission: SubmissionId,
        digest: [u8; 32],
    ) -> AppFuture<'_, Option<QuestionReceipt>> {
        Box::pin(async move {
            self.read(move |db| {
                check(db, &actor, &session)?;
                replay(db, &actor, &session, &request, &submission, Some(digest))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn reserve_question(
        &self,
        actor: Actor,
        session: HistorySession,
        submission: SubmissionId,
        receipt: QuestionReceipt,
    ) -> AppFuture<'_, QuestionAdmission> {
        Box::pin(async move {
            self.write(move |tx| reserve(tx, actor, session, submission, receipt))
                .await
                .map_err(StoreError::app)
        })
    }

    fn settle_question(
        &self,
        actor: Actor,
        session: HistorySession,
        key: QuestionKey,
        state: QuestionStatus,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                check(&tx.sql, &actor, &session)?;
                let state = match state {
                    QuestionStatus::Answered => "answered",
                    QuestionStatus::Closed => "closed",
                    QuestionStatus::Uncertain => "uncertain",
                    QuestionStatus::Unavailable => "unavailable",
                    _ => return Err(AppError::InvalidRequest.into()),
                };
                let changed = tx.sql.execute(
                    "UPDATE question_attempts SET state=?7,updated_at_ms=?8
                     WHERE session_key=?1 AND request_id=?2 AND native_turn_id=?3 AND native_item_id=?4
                       AND source_generation=?5 AND source_position=?6 AND state!=?7
                       AND ((?7='answered' AND answer_digest IS NOT NULL AND state IN ('submitting','closed','uncertain'))
                           OR (?7='closed' AND state IN ('submitting','uncertain'))
                           OR (?7 IN ('uncertain','unavailable') AND state='submitting'))",
                    params![session.target.session_key().as_str(),key.request,key.turn.as_str(),
                        key.item.as_str(),key.anchor.generation,offset(key.anchor.opening)?,state,now_ms()?],
                )?;
                if changed != 0 {
                    emit(tx, &session)?;
                }
                Ok(())
            }).await.map_err(StoreError::app)
        })
    }
}

fn check(db: &Connection, actor: &Actor, session: &HistorySession) -> Result<(), StoreError> {
    let current = history_session(
        db,
        actor,
        session.target.run_id().clone(),
        session.target.session_key().clone(),
    )?;
    if current != *session || current.binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::Conflict.into());
    }
    Ok(())
}

fn reserve(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    session: HistorySession,
    submission: SubmissionId,
    receipt: QuestionReceipt,
) -> Result<QuestionAdmission, StoreError> {
    check(&tx.sql, &actor, &session)?;
    if let Some(existing) = replay(
        &tx.sql,
        &actor,
        &session,
        &receipt.key.request,
        &submission,
        receipt.digest,
    )? {
        return Ok(QuestionAdmission::Existing(existing));
    }
    let key = &receipt.key;
    if receipt.state != QuestionStatus::Submitting || key.anchor.generation.len() != 64 {
        return Err(AppError::InvalidRequest.into());
    }
    let inserted = tx.sql.execute(
        "INSERT INTO question_attempts(session_key,request_id,native_turn_id,native_item_id,
             source_generation,source_position,blocking,actor_key,submission_id,answer_digest,state,updated_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'submitting',?11)
         ON CONFLICT(session_key,native_turn_id,native_item_id) DO NOTHING",
        params![session.target.session_key().as_str(),key.request,key.turn.as_str(),
            key.item.as_str(),key.anchor.generation,offset(key.anchor.opening)?,receipt.blocking,
            actor_key(&actor).as_slice(),submission.as_str(),receipt.digest.as_ref().map(|digest| digest.as_slice()),
            now_ms()?],
    )?;
    if inserted != 1 {
        return Err(AppError::Conflict.into());
    }
    emit(tx, &session)?;
    Ok(QuestionAdmission::Reserved(receipt))
}

fn replay(
    db: &Connection,
    actor: &Actor,
    session: &HistorySession,
    request: &str,
    submission: &SubmissionId,
    digest: Option<[u8; 32]>,
) -> Result<Option<QuestionReceipt>, StoreError> {
    let mut query = db.prepare_cached(&format!(
        "SELECT {FIELDS},session_key FROM question_attempts WHERE actor_key=?1 AND submission_id=?2"
    ))?;
    let mut rows = query.query(params![actor_key(actor).as_slice(), submission.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let found = receipt(row)?;
    if text(row, 8)? != session.target.session_key().as_str()
        || found.key.request != request
        || found.digest.is_none()
        || digest != found.digest
    {
        return Err(AppError::IdempotencyConflict.into());
    }
    Ok(Some(found))
}

fn receipt(row: &Row<'_>) -> Result<QuestionReceipt, StoreError> {
    let position: i64 = row.get(4)?;
    let digest = row
        .get::<_, Option<Vec<u8>>>(6)?
        .map(|bytes| bytes.try_into().map_err(|_| AppError::Internal))
        .transpose()?;
    let state = match text(row, 7)? {
        "submitting" => QuestionStatus::Submitting,
        "answered" => QuestionStatus::Answered,
        "closed" => QuestionStatus::Closed,
        "unavailable" => QuestionStatus::Unavailable,
        "uncertain" => QuestionStatus::Uncertain,
        _ => return Err(AppError::Internal.into()),
    };
    Ok(QuestionReceipt {
        key: QuestionKey {
            request: row.get(0)?,
            turn: parse(text(row, 1)?)?,
            item: parse(text(row, 2)?)?,
            anchor: QuestionAnchor {
                generation: row.get(3)?,
                opening: u64::try_from(position).map_err(|_| AppError::Internal)?,
            },
        },
        blocking: row.get(5)?,
        state,
        digest,
    })
}

fn offset(opening: u64) -> Result<i64, AppError> {
    i64::try_from(opening).map_err(|_| AppError::InvalidRequest)
}

fn emit(tx: &mut WriteTx<'_>, session: &HistorySession) -> Result<(), StoreError> {
    tx.emit(&[
        ResourceKey::Run(Some(session.target.run_id().clone())),
        ResourceKey::Session(Some(session.target.session_key().clone())),
    ])?;
    Ok(())
}
