//! Durable admission, claim fencing, reconciliation and exact receipt lookup.

mod admission;
mod labels;
pub(super) use admission::{admit, find_receipt, insert_intent, read_receipt, replay};
use admission::{request_digest, validate_target};
use labels::{outcome_label, recoverable_label};
mod workflow;

pub(crate) use workflow::admit_workflow;

use super::questions::async_answers;
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::queue::{lead_for_run, parse, text};
use crate::workspace::provider_label;
use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::hosts::{
    DeliveryClaim, DeliveryObservation, DeliveryOutcome, DeliveryReceipt, DeliveryRecoveryCursor,
    DeliveryReport, DeliveryWork, MessageBody, MessagePurpose, ObservedInput, RecoverableDelivery,
    RecoverableDeliveryState, ReportQuote, ReportRef,
};
use reviewq_domain::identity::{DeliveryFence, DeliveryId, NativeTurnId, Revision};
use reviewq_domain::session::{HostProvider, LeadTarget, SessionTarget};
use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, params};

const CLAIM_MS: i64 = 90_000;
const RECEIPT_COLUMNS: &str =
    "d.message_id, d.submission_id, r.review_id, d.run_id, d.session_key, d.state,
     d.created_at_ms, d.updated_at_ms, d.native_turn_id, d.native_input_id, coalesce(o.fence, 0),
     d.purpose, o.claim_kind, m.grant_id, m.grant_epoch, o.due_at_ms, coalesce(o.retry_count, 0),
     d.receipt_id, d.report_hash, d.current_receipt_id, o.reconcile_cursor, o.uncertain_turn_id, o.uncertain_input_id, d.operation";
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
    operation: String,
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
            let authorized = workflow_authorized(&tx.sql, &target, &delivery, guard.as_ref())?;
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
                if delivery.operation == async_answers::OPERATION {
                    async_answers::advance(
                        tx,
                        target.target(),
                        &delivery_id,
                        &DeliveryOutcome::Rejected,
                    )?;
                }
                emit_delivery(tx, target.target())?;
                return Ok(None);
            }
            (
                ClaimKind::Dispatch,
                "dispatching",
                dispatch_work(&tx.sql, &delivery, target.target())?,
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
        work: dispatch_work(connection, &stored, target.target())?,
        delivery_id: id,
        fence,
        target,
        purpose: stored.purpose,
    })
}

fn registered_lead(connection: &Connection, delivery: &Delivery) -> Result<LeadTarget, StoreError> {
    if delivery.operation == async_answers::OPERATION {
        let session = async_answers::binding(connection, &delivery.receipt.target)?;
        return Ok(LeadTarget::new(session.target, session.binding));
    }
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
    let DeliveryWork::Dispatch { workflow, .. } = &claim.work else {
        return Err(AppError::Forbidden.into());
    };
    let stored = matching_claim(connection, &claim)?;
    let now = now_ms()?;
    if stored.due_at_ms.is_none_or(|deadline| deadline <= now) {
        return Err(AppError::Conflict.into());
    }
    if !grant_current(connection, &stored)? {
        return Err(AppError::Forbidden.into());
    }
    if !workflow_authorized(connection, &claim.target, &stored, workflow.as_deref())? {
        return Err(AppError::Forbidden.into());
    }
    if claim.work != dispatch_work(connection, &stored, claim.target.target())? {
        return Err(AppError::Conflict.into());
    }
    Ok(())
}

fn workflow_authorized(
    db: &Connection,
    target: &LeadTarget,
    delivery: &Delivery,
    guard: Option<&reviewq_app::workflow::WorkflowGuard>,
) -> Result<bool, StoreError> {
    if delivery.operation == async_answers::OPERATION {
        return Ok(guard.is_none()
            && async_answers::review_open(db, target.target())?
            && async_answers::for_delivery(db, target.target(), &delivery.receipt.delivery_id)?
                .is_some());
    }
    match guard {
        Some(guard) if guard.binding.message_purpose() == delivery.purpose => {
            workflow::current(db, target, &delivery.receipt.submission_id, guard)
        }
        Some(_) => Ok(false),
        None => Ok(!delivery.purpose.requires_new_turn()),
    }
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
    finish_with(tx, claim, observation, CompletionAuthority::Claim)
}

/// Called only after the Claude bridge identifies a definite drop on the original offer.
pub(super) fn finish_claude_drop(
    tx: &mut WriteTx<'_>,
    claim: DeliveryClaim,
) -> Result<DeliveryReceipt, StoreError> {
    if claim.target.binding().identity().provider() != HostProvider::Claude
        || ClaimKind::from_work(&claim.work) != ClaimKind::Reconcile
    {
        return Err(AppError::Conflict.into());
    }
    finish_with(
        tx,
        claim,
        DeliveryObservation::NotSubmitted,
        CompletionAuthority::ClaudeDrop,
    )
}

enum CompletionAuthority {
    Claim,
    ClaudeDrop,
}

fn finish_with(
    tx: &mut WriteTx<'_>,
    claim: DeliveryClaim,
    observation: DeliveryObservation,
    authority: CompletionAuthority,
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
        DeliveryObservation::NotSubmitted
            if matches!(authority, CompletionAuthority::ClaudeDrop) =>
        {
            DeliveryOutcome::Queued
        }
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
        if (state == "uncertain") != (prior == "uncertain") {
            crate::workspace::advance_queue(&tx.sql)?;
            tx.emit(&[
                ResourceKey::Queue,
                ResourceKey::Review(Some(stored.receipt.target.review_id().clone())),
            ])?;
        }
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
    if stored.operation == async_answers::OPERATION {
        async_answers::advance(tx, claim.target.target(), &claim.delivery_id, &outcome)?;
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
        operation: row.get(23)?,
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

fn dispatch_work(
    connection: &Connection,
    delivery: &Delivery,
    target: &SessionTarget,
) -> Result<DeliveryWork, StoreError> {
    let id = &delivery.receipt.delivery_id;
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
        workflow: workflow::read_guard(connection, id)?.map(Box::new),
        async_question: if delivery.operation == async_answers::OPERATION {
            Some(Box::new(
                async_answers::for_delivery(connection, target, id)?
                    .ok_or(AppError::StaleRevision)?,
            ))
        } else {
            None
        },
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
