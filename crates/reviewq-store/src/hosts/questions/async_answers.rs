use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::ResourceKey;
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{
    AsyncQuestionDispatch, DeliveryOutcome, MessageBody, ObservedAsyncQuestion, QuestionAnchor,
    QuestionKey, QuestionStatus, SubmitMessage,
};
use reviewq_domain::identity::{DeliveryId, SubmissionId};
use reviewq_domain::session::{HostProvider, LeadTarget, SessionTarget};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use super::{check, offset, question_state};
use crate::StoreError;
use crate::connection::WriteTx;
use crate::history::history_session;
use crate::hosts::delivery::{find_receipt, insert_intent};
use crate::queue::{parse, text};
use crate::workspace::{actor_key, advance_queue, hash_field};

pub(in crate::hosts) const OPERATION: &str = "async_question_answer";
const FIELDS: &str =
    "q.request_id,q.native_turn_id,q.native_item_id,q.source_generation,q.source_position,
                       q.source_line_sha256,q.occurred_at_ms,q.state";

pub(crate) fn list(
    db: &Connection,
    session: &HistorySession,
    request: Option<&str>,
    active: bool,
) -> Result<Vec<ObservedAsyncQuestion>, StoreError> {
    let mut statement = db.prepare_cached(&format!(
        "SELECT {FIELDS} FROM native_async_questions q
         WHERE (q.run_id,q.session_key)=(?1,?2) AND (?3 IS NULL OR q.request_id=?3)
           AND (NOT ?4 OR q.state IN ('pending','submitting','uncertain'))
         ORDER BY (q.state IN ('pending','submitting','uncertain')) DESC,q.occurred_at_ms DESC,q.request_id LIMIT 8",
    ))?;
    let mut rows = statement.query(params![
        session.target.run_id().as_str(),
        session.target.session_key().as_str(),
        request,
        active
    ])?;
    let mut found = Vec::new();
    while let Some(row) = rows.next()? {
        found.push(evidence(row)?);
    }
    Ok(found)
}

pub(crate) fn remember(
    db: &Connection,
    session: &HistorySession,
    evidence: &ObservedAsyncQuestion,
) -> Result<bool, StoreError> {
    let state = match evidence.state {
        QuestionStatus::Pending => "pending",
        QuestionStatus::Answered => "answered",
        _ => return Err(AppError::InvalidRequest.into()),
    };
    let changed = db.execute(
        "INSERT INTO native_async_questions(run_id,session_key,request_id,native_turn_id,native_item_id,
            source_generation,source_position,source_line_sha256,occurred_at_ms,state)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(session_key,native_turn_id,native_item_id) DO UPDATE SET state='answered'
         WHERE excluded.state='answered' AND state IN ('pending','submitting','uncertain')
           AND (run_id,request_id,source_generation,source_position,source_line_sha256,occurred_at_ms)=
             (excluded.run_id,excluded.request_id,excluded.source_generation,excluded.source_position,
              excluded.source_line_sha256,excluded.occurred_at_ms)",
        params![session.target.run_id().as_str(), session.target.session_key().as_str(),
            evidence.key.request, evidence.key.turn.as_str(), evidence.key.item.as_str(),
            evidence.key.anchor.generation, offset(evidence.key.anchor.opening)?,
            evidence.line_sha256.as_str(), evidence.occurred_at_ms, state],
    )?;
    Ok(changed != 0)
}

pub(super) fn admit(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    session: HistorySession,
    submission: SubmissionId,
    question: ObservedAsyncQuestion,
    body: MessageBody,
    digest: [u8; 32],
) -> Result<QuestionStatus, StoreError> {
    check(&tx.sql, &actor, &session)?;
    let target = &session.target;
    if session.binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::Forbidden.into());
    }
    if body.as_str().len() > 48_000 {
        return Err(AppError::PayloadTooLarge.into());
    }
    let digest = request_digest(target, &question.key.request, digest);
    if let Some(state) = replay(
        &tx.sql,
        &actor,
        target,
        &submission,
        &question.key.request,
        &digest,
    )? {
        return Ok(state);
    }
    if question.state != QuestionStatus::Pending || !review_open(&tx.sql, target)? {
        return Err(AppError::PreconditionFailed.into());
    }
    let current = list(&tx.sql, &session, Some(&question.key.request), false)?
        .pop()
        .ok_or(AppError::NotFound)?;
    if current != question {
        return Err(AppError::StaleRevision.into());
    }
    let address = LeadTarget::new(target.clone(), session.binding.clone());
    crate::hosts::recovery::cancel(&tx.sql, &address, "recovery_superseded_by_user_message")?;
    let input = SubmitMessage::user_input(address, submission, body, None);
    let (delivery, _) = insert_intent(tx, &actor, &input, OPERATION, &digest, None, None)?;
    // New answers store the service delivery ID. Historical imports hold their original submission ID
    // and cannot become dispatch authority because they have no matching admitted delivery.
    let changed = tx.sql.execute(
        "UPDATE native_async_questions SET state='submitting',answer_submission_id=?4
         WHERE (run_id,session_key,request_id)=(?1,?2,?3) AND state='pending' AND answer_submission_id IS NULL",
        params![target.run_id().as_str(), target.session_key().as_str(), question.key.request, delivery.as_str()],
    )?;
    if changed != 1 {
        return Err(AppError::StaleRevision.into());
    }
    emit(tx, target)?;
    Ok(QuestionStatus::Submitting)
}

pub(super) fn replay(
    db: &Connection,
    actor: &Actor,
    target: &SessionTarget,
    submission: &SubmissionId,
    request: &str,
    digest: &[u8; 32],
) -> Result<Option<QuestionStatus>, StoreError> {
    let Some((delivery, recorded)) = find_receipt(db, &actor_key(actor), OPERATION, submission)?
    else {
        return Ok(None);
    };
    if recorded.as_slice() != digest {
        return Err(AppError::IdempotencyConflict.into());
    }
    let state = db
        .query_row(
            "SELECT state FROM native_async_questions
         WHERE (run_id,session_key,request_id,answer_submission_id)=(?1,?2,?3,?4)",
            params![
                target.run_id().as_str(),
                target.session_key().as_str(),
                request,
                delivery.as_str()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(AppError::IdempotencyConflict)?;
    Ok(Some(question_state(&state)?))
}

pub(super) fn request_digest(target: &SessionTarget, request: &str, answer: [u8; 32]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reviewq.async_question_answer.v1");
    for field in [
        target.review_id().as_str(),
        target.run_id().as_str(),
        target.session_key().as_str(),
        request,
    ] {
        hash_field(&mut digest, field.as_bytes());
    }
    digest.update(answer);
    digest.finalize().into()
}

pub(in crate::hosts) fn binding(
    db: &Connection,
    target: &SessionTarget,
) -> Result<HistorySession, StoreError> {
    let session = history_session(
        db,
        &Actor::local_operator(),
        target.run_id().clone(),
        target.session_key().clone(),
    )?;
    if session.target != *target || session.binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::Conflict.into());
    }
    Ok(session)
}

pub(in crate::hosts) fn for_delivery(
    db: &Connection,
    target: &SessionTarget,
    delivery: &DeliveryId,
) -> Result<Option<AsyncQuestionDispatch>, StoreError> {
    let mut statement = db.prepare_cached(&format!(
        "SELECT {FIELDS} FROM native_async_questions q
         WHERE (q.run_id,q.session_key,q.answer_submission_id)=(?1,?2,?3) AND q.state='submitting'",
    ))?;
    let mut rows = statement.query(params![
        target.run_id().as_str(),
        target.session_key().as_str(),
        delivery.as_str()
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let question = evidence(row)?;
    if rows.next()?.is_some() {
        return Err(AppError::Conflict.into());
    }
    Ok(Some(AsyncQuestionDispatch {
        session: binding(db, target)?,
        question,
    }))
}

pub(in crate::hosts) fn review_open(
    db: &Connection,
    target: &SessionTarget,
) -> Result<bool, StoreError> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM reviews r JOIN configured_projects p USING(origin,project_id)
                       WHERE r.review_id=?1 AND r.state='open')",
        [target.review_id().as_str()],
        |row| row.get(0),
    )?)
}

pub(in crate::hosts) fn advance(
    tx: &mut WriteTx<'_>,
    target: &SessionTarget,
    delivery: &DeliveryId,
    outcome: &DeliveryOutcome,
) -> Result<(), StoreError> {
    let state = match outcome {
        DeliveryOutcome::Started(_) => "answered",
        DeliveryOutcome::Rejected => "unavailable",
        DeliveryOutcome::Uncertain => "uncertain",
        _ => return Ok(()),
    };
    if tx.sql.execute(
        "UPDATE native_async_questions SET state=?4
         WHERE (run_id,session_key,answer_submission_id)=(?1,?2,?3)
           AND state IN ('submitting','uncertain') AND state IS NOT ?4",
        params![
            target.run_id().as_str(),
            target.session_key().as_str(),
            delivery.as_str(),
            state
        ],
    )? != 0
    {
        emit(tx, target)?;
    }
    Ok(())
}

fn emit(tx: &mut WriteTx<'_>, target: &SessionTarget) -> Result<(), StoreError> {
    advance_queue(&tx.sql)?;
    tx.emit(&[
        ResourceKey::Queue,
        ResourceKey::Review(Some(target.review_id().clone())),
        ResourceKey::Run(Some(target.run_id().clone())),
        ResourceKey::Session(Some(target.session_key().clone())),
        ResourceKey::History(Some(target.session_key().clone())),
    ])?;
    Ok(())
}

fn evidence(row: &Row<'_>) -> Result<ObservedAsyncQuestion, StoreError> {
    Ok(ObservedAsyncQuestion {
        key: QuestionKey {
            request: row.get(0)?,
            turn: parse(text(row, 1)?)?,
            item: parse(text(row, 2)?)?,
            anchor: QuestionAnchor {
                generation: row.get(3)?,
                opening: u64::try_from(row.get::<_, i64>(4)?).map_err(|_| AppError::Internal)?,
            },
        },
        line_sha256: parse(text(row, 5)?)?,
        occurred_at_ms: row.get(6)?,
        state: question_state(text(row, 7)?)?,
    })
}
