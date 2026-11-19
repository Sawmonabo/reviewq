//! Durable admission, claim fencing, reconciliation and exact receipt lookup.

mod labels;
use labels::{outcome_label, purpose_label, recoverable_label};
mod workflow;

pub(crate) use workflow::admit_workflow;

use reviewq_app::access::{Actor, ActorScope, AppError};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::hosts::{
    DeliveryClaim, DeliveryObservation, DeliveryOutcome, DeliveryReceipt, DeliveryRecoveryCursor,
    DeliveryReport, DeliveryWork, MessageBody, MessagePurpose, MessageReplay, ObservedInput,
    RecoverableDelivery, RecoverableDeliveryState, ReportQuote, ReportRef, SubmitMessage,
};
use reviewq_domain::identity::{DeliveryFence, DeliveryId, NativeTurnId, Revision, SubmissionId};
use reviewq_domain::session::{HostProvider, LeadTarget, SessionTarget};
use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::allow_target;
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::{lead_for_run, parse, text};
use crate::reports::authorize_feedback;
use crate::workspace::{actor_key, hash_field, provider_label};

const OPERATION: &str = "message";
const CLAIM_MS: i64 = 90_000;
const RECEIPT_COLUMNS: &str =
    "d.message_id, d.submission_id, r.review_id, d.run_id, d.session_key, d.state,
     d.created_at_ms, d.updated_at_ms, d.native_turn_id, d.native_input_id, coalesce(o.fence, 0),
     d.purpose, o.claim_kind, m.grant_id, m.grant_epoch, o.due_at_ms, coalesce(o.retry_count, 0),
     d.receipt_id, d.report_hash, d.current_receipt_id, o.reconcile_cursor, o.uncertain_turn_id, o.uncertain_input_id";
const RECEIPT_JOIN: &str =
    "messages d JOIN runs r ON r.run_id=d.run_id
     JOIN mutation_receipts m ON m.actor_key=d.actor_key AND m.operation=d.operation AND m.submission_id=d.submission_id
     LEFT JOIN outbox o ON o.delivery_id=d.message_id";
pub(super) const SCHEDULE_FROM: &str = "FROM outbox o JOIN messages m ON m.message_id=o.delivery_id
    JOIN sessions s ON s.run_id=m.run_id AND s.session_key=m.session_key
    WHERE s.provider=?1 AND NOT EXISTS (
        SELECT 1 FROM messages prior JOIN outbox pending ON pending.delivery_id=prior.message_id
        WHERE prior.run_id=m.run_id AND prior.session_key=m.session_key
          AND prior.message_id<>m.message_id
          AND prior.state IN ('queued','dispatching','uncertain')
          AND ((prior.state<>'queued' AND m.state='queued')
            OR (prior.state='queued' AND m.state='queued'
                AND prior.purpose IN ('user','report_feedback') AND m.purpose NOT IN ('user','report_feedback'))
            OR ((prior.state='queued')=(m.state='queued')
              AND ((prior.purpose IN ('user','report_feedback'))=(m.purpose IN ('user','report_feedback'))
                   OR prior.state<>'queued')
              AND (prior.created_at_ms, prior.rowid) < (m.created_at_ms, m.rowid)))
    )";

struct Delivery {
    receipt: DeliveryReceipt,
    fence: DeliveryFence,
    purpose: MessagePurpose,
    claim: Option<ClaimKind>,
    grant: Option<(String, Revision)>,
    due_at_ms: Option<i64>,
    retry_count: u8,
    reconcile_cursor: Option<String>,
    raced_input: Option<ObservedInput>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ClaimKind {
    Dispatch,
    Reconcile,
}

impl ClaimKind {
    fn from_work(work: &DeliveryWork) -> Self {
        match work {
            DeliveryWork::Dispatch { .. } => Self::Dispatch,
            DeliveryWork::Reconcile { .. } => Self::Reconcile,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Reconcile => "reconcile",
        }
    }
}

pub(super) fn admit(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    input: SubmitMessage,
) -> Result<Snapshot<DeliveryReceipt>, StoreError> {
    let registered = validate_target(&tx.sql, &actor, input.target.target())?;
    if registered != input.target {
        return Err(AppError::Conflict.into());
    }
    if matches!(
        input.purpose,
        MessagePurpose::Publication
            | MessagePurpose::ThreadFollowup
            | MessagePurpose::CodexRecovery
    ) || (input.purpose == MessagePurpose::ReportFeedback) != input.report_ref.is_some()
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

fn insert_intent(
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

pub(super) fn replay(
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

pub(super) fn read_receipt(
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

fn validate_target(
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

fn find_receipt(
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

fn request_digest(input: &SubmitMessage) -> [u8; 32] {
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

pub(super) fn recoverable(
    connection: &Connection,
    provider: HostProvider,
    after: Option<DeliveryRecoveryCursor>,
    limit: u16,
) -> Result<Vec<RecoverableDelivery>, StoreError> {
    let (time, id) = after
        .as_ref()
        .map_or((i64::MIN, ""), |v| (v.due_at_ms, v.delivery_id.as_str()));
    let sql = format!(
        "SELECT o.due_at_ms, o.delivery_id, m.state {SCHEDULE_FROM}
         AND (o.due_at_ms, o.delivery_id) > (?2, ?3) AND o.due_at_ms<=?4
         ORDER BY o.due_at_ms, o.delivery_id LIMIT ?5"
    );
    let mut select = connection.prepare_cached(&sql)?;
    let mut rows = select.query(params![
        provider_label(provider),
        time,
        id,
        now_ms()?,
        limit
    ])?;
    let mut values = Vec::new();
    while let Some(row) = rows.next()? {
        values.push(RecoverableDelivery {
            cursor: DeliveryRecoveryCursor {
                due_at_ms: row.get(0)?,
                delivery_id: parse(text(row, 1)?)?,
            },
            state: match text(row, 2)? {
                "queued" => RecoverableDeliveryState::Queued,
                "dispatching" => RecoverableDeliveryState::Dispatching,
                "uncertain" => RecoverableDeliveryState::Uncertain,
                _ => return Err(AppError::Internal.into()),
            },
        });
    }
    Ok(values)
}

pub(super) fn next_due(
    connection: &Connection,
    provider: HostProvider,
) -> Result<Option<i64>, StoreError> {
    let sql =
        format!("SELECT o.due_at_ms {SCHEDULE_FROM} ORDER BY o.due_at_ms, o.delivery_id LIMIT 1");
    Ok(connection
        .prepare_cached(&sql)?
        .query_row([provider_label(provider)], |row| row.get(0))
        .optional()?)
}

pub(super) fn claim(
    tx: &mut WriteTx<'_>,
    delivery_id: DeliveryId,
    expected: RecoverableDeliveryState,
) -> Result<Option<DeliveryClaim>, StoreError> {
    let Some(delivery) = by_id(&tx.sql, &delivery_id)? else {
        return Ok(None);
    };
    let prior = recoverable_label(expected);
    let time = now_ms()?;
    if outcome_label(&delivery.receipt.outcome) != prior
        || delivery.due_at_ms.is_none_or(|due| due > time)
    {
        return Ok(None);
    }
    let target = registered_lead(&tx.sql, &delivery)?;
    let address = format!("SELECT 1 {SCHEDULE_FROM} AND o.delivery_id=?2");
    if tx
        .sql
        .prepare_cached(&address)?
        .query_row(
            params![
                provider_label(target.binding().identity().provider()),
                delivery_id.as_str()
            ],
            |_| Ok(()),
        )
        .optional()?
        .is_none()
    {
        return Ok(None);
    }
    let (kind, state, work) = match expected {
        RecoverableDeliveryState::Queued => {
            let guard = workflow::read_guard(&tx.sql, &delivery_id)?;
            let needs_guard = matches!(
                delivery.purpose,
                MessagePurpose::ThreadFollowup | MessagePurpose::Publication
            );
            let authorized = match guard.as_ref() {
                Some(guard) if needs_guard => {
                    workflow::current(&tx.sql, &target, &delivery.receipt.submission_id, guard)?
                }
                None if !needs_guard => true,
                _ => false,
            };
            if !grant_current(&tx.sql, &delivery)? || !authorized {
                tx.sql.execute(
                    "UPDATE messages SET state='rejected', updated_at_ms=max(updated_at_ms, ?2)
                     WHERE message_id=?1 AND state='queued'",
                    params![delivery_id.as_str(), time],
                )?;
                tx.delivery_changed = true;
                tx.sql.execute(
                    "DELETE FROM outbox WHERE delivery_id=?1",
                    [delivery_id.as_str()],
                )?;
                if let Some(guard) = &guard {
                    workflow::advance(
                        tx,
                        &target,
                        &delivery.receipt.submission_id,
                        guard,
                        &DeliveryOutcome::Rejected,
                        time,
                    )?;
                }
                emit_delivery(tx, target.target())?;
                return Ok(None);
            }
            (
                ClaimKind::Dispatch,
                "dispatching",
                dispatch_work(&tx.sql, &delivery_id)?,
            )
        }
        RecoverableDeliveryState::Dispatching | RecoverableDeliveryState::Uncertain => (
            ClaimKind::Reconcile,
            prior,
            DeliveryWork::Reconcile {
                cursor: delivery.reconcile_cursor,
            },
        ),
    };
    let fence = delivery.fence.checked_add(1).ok_or(AppError::Internal)?;
    let changed = tx.sql.execute(
        "UPDATE outbox SET fence=?3, claim_kind=?4, due_at_ms=?5
         WHERE delivery_id=?1 AND fence=?2",
        params![
            delivery_id.as_str(),
            delivery.fence.as_sqlite_integer(),
            fence.as_sqlite_integer(),
            kind.label(),
            time.saturating_add(CLAIM_MS),
        ],
    )?;
    if changed != 1 {
        return Err(AppError::Conflict.into());
    }
    if state != prior {
        tx.sql.execute(
            "UPDATE messages SET state=?2, updated_at_ms=max(updated_at_ms, ?3) WHERE message_id=?1",
            params![delivery_id.as_str(), state, time],
        )?;
        emit_delivery(tx, target.target())?;
    }
    Ok(Some(DeliveryClaim {
        delivery_id,
        fence,
        target,
        purpose: delivery.purpose,
        work,
    }))
}

pub(super) fn existing_dispatch(
    connection: &Connection,
    id: DeliveryId,
    fence: DeliveryFence,
    target: LeadTarget,
) -> Result<DeliveryClaim, StoreError> {
    let stored = by_id(connection, &id)?.ok_or(AppError::NotFound)?;
    if stored.fence != fence
        || (!matches!(stored.receipt.outcome, DeliveryOutcome::Uncertain)
            && stored.claim != Some(ClaimKind::Dispatch))
        || stored.receipt.target != *target.target()
    {
        return Err(AppError::Conflict.into());
    }
    Ok(DeliveryClaim {
        work: dispatch_work(connection, &id)?,
        delivery_id: id,
        fence,
        target,
        purpose: stored.purpose,
    })
}

fn registered_lead(connection: &Connection, delivery: &Delivery) -> Result<LeadTarget, StoreError> {
    let target =
        lead_for_run(connection, delivery.receipt.target.run_id())?.ok_or(AppError::Internal)?;
    if target.target() != &delivery.receipt.target {
        return Err(AppError::Internal.into());
    }
    Ok(target)
}

fn grant_current(connection: &Connection, delivery: &Delivery) -> Result<bool, StoreError> {
    let Some((id, epoch)) = &delivery.grant else {
        return Ok(true);
    };
    let current = connection.query_row(
        "SELECT 1 FROM repo_grants g
         JOIN configured_projects issued ON issued.origin=g.origin AND issued.project_id=g.project_id
         JOIN reviews r ON r.review_id=?3
         JOIN configured_projects target ON target.origin=r.origin AND target.project_id=r.project_id
         WHERE g.grant_id=?1 AND g.epoch=?2 AND g.active=1",
        params![id, epoch.as_sqlite_integer(), delivery.receipt.target.review_id().as_str()],
        |_| Ok(()),
    ).optional()?;
    Ok(current.is_some())
}

pub(super) fn authorize_dispatch(
    connection: &Connection,
    claim: DeliveryClaim,
) -> Result<(), StoreError> {
    if ClaimKind::from_work(&claim.work) != ClaimKind::Dispatch {
        return Err(AppError::Forbidden.into());
    }
    let stored = matching_claim(connection, &claim)?;
    let now = now_ms()?;
    if stored.due_at_ms.is_none_or(|deadline| deadline <= now) {
        return Err(AppError::Conflict.into());
    }
    if !grant_current(connection, &stored)? {
        return Err(AppError::Forbidden.into());
    }
    if claim.work != dispatch_work(connection, &claim.delivery_id)? {
        return Err(AppError::Conflict.into());
    }
    if let DeliveryWork::Dispatch {
        workflow: Some(guard),
        ..
    } = &claim.work
        && !workflow::current(
            connection,
            &claim.target,
            &stored.receipt.submission_id,
            guard,
        )?
    {
        return Err(AppError::Forbidden.into());
    }
    Ok(())
}

pub(super) fn workflow_requirements(
    db: &Connection,
    claim: DeliveryClaim,
) -> Result<reviewq_app::workflow::WorkflowRequirements, StoreError> {
    let DeliveryWork::Dispatch {
        workflow: Some(guard),
        ..
    } = &claim.work
    else {
        return Err(AppError::Forbidden.into());
    };
    let target = claim.target.clone();
    let guard = guard.clone();
    authorize_dispatch(db, claim)?;
    workflow::requirements(db, &target, &guard)
}

pub(crate) fn unresolved_workflow_input(
    db: &Connection,
    lead: &LeadTarget,
    turn: &NativeTurnId,
) -> Result<Option<AppError>, StoreError> {
    let ambiguous = db
        .query_row(
            "SELECT o.uncertain_turn_id FROM messages m JOIN outbox o ON o.delivery_id=m.message_id
         JOIN workflow_dispatch w USING(delivery_id)
         WHERE m.run_id=?1 AND m.session_key=?2 AND m.state IN ('dispatching','uncertain')
           AND (o.uncertain_turn_id=?3 OR o.uncertain_turn_id IS NULL)
         ORDER BY o.uncertain_turn_id DESC LIMIT 1",
            params![
                lead.run_id().as_str(),
                lead.session_key().as_str(),
                turn.as_str()
            ],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(ambiguous.map(|known_turn| {
        if known_turn.is_some() {
            AppError::Conflict
        } else {
            AppError::Busy
        }
    }))
}

pub(super) fn finish(
    tx: &mut WriteTx<'_>,
    claim: DeliveryClaim,
    observation: DeliveryObservation,
) -> Result<DeliveryReceipt, StoreError> {
    let mut stored = matching_claim(&tx.sql, &claim)?;
    let scan = match &observation {
        DeliveryObservation::ReconcileFrom(cursor)
            if !cursor.is_empty() && cursor.len() <= 2048 =>
        {
            Some(cursor.clone())
        }
        DeliveryObservation::ReconcileFrom(_) => return Err(AppError::InvalidRequest.into()),
        _ => None,
    };
    let uncertain_proof = match &observation {
        DeliveryObservation::UncertainInput { delivery_id, input }
            if delivery_id == &claim.delivery_id
                && claim.purpose.requires_new_turn()
                && stored
                    .raced_input
                    .as_ref()
                    .is_none_or(|previous| previous == input) =>
        {
            Some(input.clone())
        }
        DeliveryObservation::UncertainInput { .. } => return Err(AppError::Conflict.into()),
        _ => None,
    };
    if let DeliveryObservation::RecoveryAllowanceChanged(ref allowance) = observation {
        crate::hosts::recovery::refresh_allowance(&tx.sql, &claim, allowance)?;
    }
    let outcome = match observation {
        DeliveryObservation::NotSubmitted | DeliveryObservation::RecoveryAllowanceChanged(_)
            if ClaimKind::from_work(&claim.work) == ClaimKind::Dispatch =>
        {
            DeliveryOutcome::Queued
        }
        DeliveryObservation::NotSubmitted | DeliveryObservation::RecoveryAllowanceChanged(_) => {
            return Err(AppError::Conflict.into());
        }
        DeliveryObservation::Started { delivery_id, input } => {
            if delivery_id != claim.delivery_id
                || stored
                    .raced_input
                    .as_ref()
                    .is_some_and(|previous| previous != &input)
            {
                return Err(AppError::Conflict.into());
            }
            DeliveryOutcome::Started(input)
        }
        DeliveryObservation::Rejected => DeliveryOutcome::Rejected,
        DeliveryObservation::Uncertain
        | DeliveryObservation::ReconcileFrom(_)
        | DeliveryObservation::UncertainInput { .. } => DeliveryOutcome::Uncertain,
    };
    if stored.raced_input.is_some()
        && matches!(outcome, DeliveryOutcome::Queued | DeliveryOutcome::Rejected)
    {
        return Err(AppError::Conflict.into());
    }
    let new_raced_input = uncertain_proof.is_some() && stored.raced_input.is_none();
    let state = outcome_label(&outcome);
    let prior = outcome_label(&stored.receipt.outcome);
    let now = now_ms()?;
    let receipt_changed = stored.receipt.outcome != outcome || new_raced_input;
    let timestamp = if receipt_changed {
        now.max(stored.receipt.updated_at_ms)
    } else {
        stored.receipt.updated_at_ms
    };
    if receipt_changed {
        let (turn, item) = match &outcome {
            DeliveryOutcome::Started(input) => {
                (Some(input.turn_id.as_str()), Some(input.entry_id.as_str()))
            }
            _ => (None, None),
        };
        let changed = tx.sql.execute(
            "UPDATE messages SET state=?2, updated_at_ms=?3, native_turn_id=?4, native_input_id=?5,
                outcome_detail=CASE WHEN ?7 THEN 'native_turn_start_unproved' WHEN ?2='started' THEN NULL ELSE outcome_detail END
             WHERE message_id=?1 AND state=?6",
            params![claim.delivery_id.as_str(), state, timestamp, turn, item, prior, new_raced_input],
        ).map_err(|error| {
            if error.sqlite_error_code() == Some(ErrorCode::ConstraintViolation) {
                StoreError::from(AppError::Conflict)
            } else {
                error.into()
            }
        })?;
        if changed != 1 {
            return Err(AppError::Conflict.into());
        }
        emit_delivery(tx, &stored.receipt.target)?;
    }
    match outcome {
        DeliveryOutcome::Started(_) | DeliveryOutcome::Rejected => {
            tx.sql.execute(
                "DELETE FROM outbox WHERE delivery_id=?1",
                [claim.delivery_id.as_str()],
            )?;
        }
        _ => {
            let (retry_count, delay) = if scan.is_some() {
                (stored.retry_count, 250)
            } else {
                let count = stored.retry_count.saturating_add(1).min(7);
                (count, 1_000_i64 << (count - 1))
            };
            let due_at_ms = now.saturating_add(delay);
            tx.sql.execute(
                "UPDATE outbox SET claim_kind=NULL,due_at_ms=?2,retry_count=?3,reconcile_cursor=?4,
                    uncertain_turn_id=coalesce(?5,uncertain_turn_id),uncertain_input_id=coalesce(?6,uncertain_input_id)
                 WHERE delivery_id=?1",
                params![claim.delivery_id.as_str(),due_at_ms,retry_count,scan,
                    uncertain_proof.as_ref().map(|input|input.turn_id.as_str()),
                    uncertain_proof.as_ref().map(|input|input.entry_id.as_str())],
            )?;
        }
    }
    if let Some(guard) = workflow::read_guard(&tx.sql, &claim.delivery_id)? {
        workflow::advance(
            tx,
            &claim.target,
            &stored.receipt.submission_id,
            &guard,
            &outcome,
            now,
        )?;
    }
    tx.delivery_changed = true;
    stored.receipt.outcome = outcome;
    stored.receipt.updated_at_ms = timestamp;
    Ok(stored.receipt)
}

fn matching_claim(connection: &Connection, claim: &DeliveryClaim) -> Result<Delivery, StoreError> {
    let delivery = by_id(connection, &claim.delivery_id)?.ok_or(AppError::NotFound)?;
    let kind = ClaimKind::from_work(&claim.work);
    if delivery.claim != Some(kind)
        || delivery.fence != claim.fence
        || delivery.purpose != claim.purpose
        || registered_lead(connection, &delivery)? != claim.target
    {
        return Err(AppError::Conflict.into());
    }
    Ok(delivery)
}

fn by_id(connection: &Connection, delivery: &DeliveryId) -> Result<Option<Delivery>, StoreError> {
    let query = format!("SELECT {RECEIPT_COLUMNS} FROM {RECEIPT_JOIN} WHERE d.message_id=?1");
    let mut statement = connection.prepare_cached(&query)?;
    let mut rows = statement.query([delivery.as_str()])?;
    rows.next()?.map(delivery_row).transpose()
}

fn delivery_row(row: &Row<'_>) -> Result<Delivery, StoreError> {
    let outcome = match text(row, 5)? {
        "queued" => DeliveryOutcome::Queued,
        "dispatching" => DeliveryOutcome::Dispatching,
        "started" => DeliveryOutcome::Started(ObservedInput {
            turn_id: parse(text(row, 8)?)?,
            entry_id: parse(text(row, 9)?)?,
        }),
        "rejected" => DeliveryOutcome::Rejected,
        "uncertain" => DeliveryOutcome::Uncertain,
        _ => return Err(AppError::Internal.into()),
    };
    let purpose = match text(row, 11)? {
        "user" => MessagePurpose::User,
        "report_feedback" => MessagePurpose::ReportFeedback,
        "publication" => MessagePurpose::Publication,
        "thread_followup" => MessagePurpose::ThreadFollowup,
        "codex_recovery" => MessagePurpose::CodexRecovery,
        _ => return Err(AppError::Internal.into()),
    };
    let claim = match row.get::<_, Option<String>>(12)?.as_deref() {
        Some("dispatch") => Some(ClaimKind::Dispatch),
        Some("reconcile") => Some(ClaimKind::Reconcile),
        None => None,
        _ => return Err(AppError::Internal.into()),
    };
    let grant = row
        .get::<_, Option<String>>(13)?
        .zip(row.get::<_, Option<i64>>(14)?)
        .map(|(id, epoch)| Revision::try_from(epoch).map(|epoch| (id, epoch)))
        .transpose()
        .map_err(|_| AppError::Internal)?;
    let report_ref = row
        .get::<_, Option<String>>(17)?
        .map(|receipt| {
            Ok::<_, StoreError>(DeliveryReport {
                receipt_id: parse(&receipt)?,
                report_hash: parse(text(row, 18)?)?,
                current_receipt_id: row
                    .get::<_, Option<String>>(19)?
                    .as_deref()
                    .map(parse)
                    .transpose()?,
            })
        })
        .transpose()?;
    let receipt = DeliveryReceipt {
        delivery_id: parse(text(row, 0)?)?,
        submission_id: parse(text(row, 1)?)?,
        target: SessionTarget::new(
            parse(text(row, 2)?)?,
            parse(text(row, 3)?)?,
            parse(text(row, 4)?)?,
        ),
        outcome,
        report_ref,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    };
    let fence = DeliveryFence::try_from(row.get::<_, i64>(10)?).map_err(|_| AppError::Internal)?;
    Ok(Delivery {
        receipt,
        fence,
        purpose,
        claim,
        grant,
        due_at_ms: row.get(15)?,
        retry_count: row.get(16)?,
        reconcile_cursor: row.get(20)?,
        raced_input: row
            .get::<_, Option<String>>(21)?
            .map(|turn| {
                Ok::<_, StoreError>(ObservedInput {
                    turn_id: parse(&turn)?,
                    entry_id: parse(text(row, 22)?)?,
                })
            })
            .transpose()?,
    })
}

fn dispatch_work(connection: &Connection, id: &DeliveryId) -> Result<DeliveryWork, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT body, receipt_id, report_hash, quoted_text, quoted_offset FROM messages WHERE message_id=?1"
    )?;
    let mut rows = statement.query([id.as_str()])?;
    let row = rows.next()?.ok_or(AppError::Internal)?;
    let text_value = MessageBody::new(text(row, 0)?).map_err(|_| AppError::Internal)?;
    let report_ref = match (
        row.get::<_, Option<String>>(1)?,
        row.get::<_, Option<String>>(2)?,
    ) {
        (Some(receipt), Some(hash)) => {
            let quote = match (
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ) {
                (Some(quoted), Some(offset)) => {
                    let value = MessageBody::new(&quoted).map_err(|_| AppError::Internal)?;
                    let offset = u64::try_from(offset).map_err(|_| AppError::Internal)?;
                    Some(ReportQuote::new(value, offset).map_err(|_| AppError::Internal)?)
                }
                (None, None) => None,
                _ => return Err(AppError::Internal.into()),
            };
            Some(ReportRef {
                receipt_id: parse(&receipt)?,
                report_hash: parse(&hash)?,
                quote,
            })
        }
        (None, None) => None,
        _ => return Err(AppError::Internal.into()),
    };
    Ok(DeliveryWork::Dispatch {
        text: text_value,
        report_ref,
        workflow: workflow::read_guard(connection, id)?,
    })
}

fn emit_delivery(tx: &mut WriteTx<'_>, target: &SessionTarget) -> Result<(), StoreError> {
    tx.emit(&[
        ResourceKey::Run(Some(target.run_id().clone())),
        ResourceKey::Session(Some(target.session_key().clone())),
        ResourceKey::History(Some(target.session_key().clone())),
    ])?;
    Ok(())
}
