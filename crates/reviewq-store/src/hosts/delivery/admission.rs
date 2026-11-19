//! One source of truth for durable user and automatic message admission.

use reviewq_app::access::{Actor, ActorScope, AppError};
use reviewq_app::events::Snapshot;
use reviewq_app::hosts::{
    DeliveryOutcome, DeliveryReceipt, DeliveryReport, MessagePurpose, MessageReplay, SubmitMessage,
};
use reviewq_domain::identity::{DeliveryId, Revision, SubmissionId};
use reviewq_domain::session::{LeadTarget, SessionTarget};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::labels::purpose_label;
use super::{by_id, emit_delivery};
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::hosts::allow_target;
use crate::queue::{lead_for_run, parse};
use crate::reports::authorize_feedback;
use crate::workspace::{actor_key, hash_field};

const OPERATION: &str = "message";

pub(in crate::hosts) fn admit(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    input: SubmitMessage,
) -> Result<Snapshot<DeliveryReceipt>, StoreError> {
    let registered = validate_target(&tx.sql, &actor, input.target.target())?;
    if registered != input.target {
        return Err(AppError::Conflict.into());
    }
    if input.purpose.requires_new_turn()
        || (input.purpose == MessagePurpose::ReportFeedback) != input.report_ref.is_some()
    {
        return Err(AppError::InvalidRequest.into());
    }
    let digest = request_digest(&input);
    let key = actor_key(&actor);
    if let Some((delivery, existing)) =
        find_receipt(&tx.sql, &key, OPERATION, &input.submission_id)?
    {
        if digest.as_slice() != existing {
            return Err(AppError::IdempotencyConflict.into());
        }
        return snapshot(
            &tx.sql,
            by_id(&tx.sql, &delivery)?
                .ok_or(AppError::Internal)?
                .receipt,
        );
    }
    crate::hosts::recovery::cancel(
        &tx.sql,
        &input.target,
        "recovery_superseded_by_user_message",
    )?;
    let current_report = input
        .report_ref
        .as_ref()
        .map(|report| authorize_feedback(&tx.sql, input.target.run_id(), report))
        .transpose()?
        .flatten();
    let (delivery_id, time) = insert_intent(
        tx,
        &actor,
        &input,
        OPERATION,
        &digest,
        current_report.as_ref(),
        None,
    )?;
    emit_delivery(tx, input.target.target())?;
    snapshot(
        &tx.sql,
        DeliveryReceipt {
            delivery_id,
            submission_id: input.submission_id,
            target: input.target.target().clone(),
            outcome: DeliveryOutcome::Queued,
            report_ref: input.report_ref.map(|reference| DeliveryReport {
                receipt_id: reference.receipt_id,
                report_hash: reference.report_hash,
                current_receipt_id: current_report,
            }),
            created_at_ms: time,
            updated_at_ms: time,
        },
    )
}

pub(super) fn insert_intent(
    tx: &mut WriteTx<'_>,
    actor: &Actor,
    input: &SubmitMessage,
    operation: &str,
    digest: &[u8; 32],
    current_report: Option<&reviewq_domain::identity::ReceiptId>,
    batch_id: Option<&str>,
) -> Result<(DeliveryId, i64), StoreError> {
    let delivery_id: DeliveryId = parse(&Uuid::new_v4().to_string())?;
    let key = actor_key(actor);
    let grant = match actor.scope() {
        ActorScope::CheckoutGrant(grant) => Some(grant.as_str()),
        _ => None,
    };
    tx.sql.execute(
        "INSERT INTO mutation_receipts(actor_key, operation, submission_id, request_digest, grant_id, grant_epoch)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![key.as_slice(), operation, input.submission_id.as_str(), digest.as_slice(),
                grant, actor.grant_epoch().map(Revision::as_sqlite_integer)],
    )?;
    let quote = input
        .report_ref
        .as_ref()
        .and_then(|reference| reference.quote.as_ref());
    let offset = quote
        .map(|quote| i64::try_from(quote.offset()))
        .transpose()
        .map_err(|_| AppError::Internal)?;
    let time = now_ms()?;
    tx.sql.execute(
        "INSERT INTO messages(message_id, actor_key, operation, submission_id, run_id, session_key, purpose,
                             body, receipt_id, report_hash, current_receipt_id, quoted_text, quoted_offset,
                             state, created_at_ms, updated_at_ms, followup_batch_id)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'queued', ?14, ?14, ?15)",
        params![
            delivery_id.as_str(), key.as_slice(), operation, input.submission_id.as_str(),
            input.target.run_id().as_str(), input.target.session_key().as_str(), purpose_label(input.purpose),
            input.text.as_str(), input.report_ref.as_ref().map(|r| r.receipt_id.as_str()),
            input.report_ref.as_ref().map(|r| r.report_hash.as_str()),
            current_report.map(|receipt| receipt.as_str()), quote.map(|q| q.text().as_str()), offset, time, batch_id,
        ],
    )?;
    tx.sql.execute(
        "INSERT INTO outbox(delivery_id, due_at_ms) VALUES(?1, ?2)",
        params![delivery_id.as_str(), time],
    )?;
    tx.delivery_changed = true;
    Ok((delivery_id, time))
}

pub(in crate::hosts) fn replay(
    db: &Connection,
    actor: Actor,
    input: MessageReplay,
) -> Result<Option<Snapshot<DeliveryReceipt>>, StoreError> {
    let Some((id, stored_digest)) =
        find_receipt(db, &actor_key(&actor), OPERATION, &input.submission)?
    else {
        return Ok(None);
    };
    let receipt = by_id(db, &id)?.ok_or(AppError::Internal)?.receipt;
    let lead = validate_target(db, &actor, &receipt.target)?;
    if lead.review_id() != &input.review
        || input
            .session
            .as_ref()
            .is_some_and(|key| key != lead.session_key())
        || request_digest(&SubmitMessage::user_input(
            lead,
            input.submission,
            input.text,
            input.report,
        ))
        .as_slice()
            != stored_digest
    {
        return Err(AppError::IdempotencyConflict.into());
    }
    snapshot(db, receipt).map(Some)
}

pub(in crate::hosts) fn read_receipt(
    connection: &Connection,
    actor: Actor,
    target: SessionTarget,
    submission_id: SubmissionId,
) -> Result<Snapshot<DeliveryReceipt>, StoreError> {
    validate_target(connection, &actor, &target)?;
    let (id, _) = find_receipt(connection, &actor_key(&actor), OPERATION, &submission_id)?
        .ok_or(AppError::NotFound)?;
    let receipt = by_id(connection, &id)?.ok_or(AppError::Internal)?.receipt;
    if receipt.target != target {
        return Err(AppError::NotFound.into());
    }
    snapshot(connection, receipt)
}

pub(super) fn validate_target(
    connection: &Connection,
    actor: &Actor,
    target: &SessionTarget,
) -> Result<LeadTarget, StoreError> {
    let lead = lead_for_run(connection, target.run_id())?.ok_or(AppError::NotFound)?;
    if lead.target() != target {
        return Err(AppError::NotFound.into());
    }
    allow_target(connection, actor, &lead)?;
    Ok(lead)
}

pub(super) fn find_receipt(
    connection: &Connection,
    key: &[u8; 32],
    operation: &str,
    submission: &SubmissionId,
) -> Result<Option<(DeliveryId, Vec<u8>)>, StoreError> {
    let row = connection.query_row(
        "SELECT d.message_id, m.request_digest FROM mutation_receipts m
         JOIN messages d ON d.actor_key=m.actor_key AND d.operation=m.operation AND d.submission_id=m.submission_id
         WHERE m.actor_key=?1 AND m.operation=?2 AND m.submission_id=?3",
        params![key.as_slice(), operation, submission.as_str()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
    ).optional()?;
    row.map(|(id, digest)| Ok((parse(&id)?, digest)))
        .transpose()
}

pub(super) fn request_digest(input: &SubmitMessage) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reviewq.message.v1");
    let mut field = |value: &str| hash_field(&mut digest, value.as_bytes());
    field(input.target.review_id().as_str());
    field(input.target.run_id().as_str());
    field(input.target.session_key().as_str());
    field(purpose_label(input.purpose));
    field(input.text.as_str());
    if let Some(reference) = &input.report_ref {
        field(reference.receipt_id.as_str());
        field(reference.report_hash.as_str());
        if let Some(quote) = &reference.quote {
            field(quote.text().as_str());
            digest.update(quote.offset().to_be_bytes());
        }
    }
    digest.finalize().into()
}
