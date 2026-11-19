use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::ResourceKey;
use reviewq_app::hosts::{DeliveryOutcome, MessagePurpose, SubmitMessage};
use reviewq_app::workflow::{ReadyWorkflow, WorkflowAdmission, WorkflowBinding, WorkflowGuard};
use reviewq_domain::identity::{DeliveryId, Revision, SubmissionId};
use reviewq_domain::session::LeadTarget;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use super::{emit_delivery, find_receipt, insert_intent, request_digest, validate_target};
use crate::StoreError;
use crate::connection::WriteTx;
use crate::queue::{parse, text};
use crate::workspace::{actor_key, advance_queue, hash_field, settings_revision};

pub(crate) fn admit_workflow(
    tx: &mut WriteTx<'_>,
    command: WorkflowAdmission,
) -> Result<DeliveryId, StoreError> {
    let guard = WorkflowGuard {
        binding: command.binding,
        settings: command.settings,
        head: command.head,
        ready: command.ready,
    };
    let (operation, purpose, batch, action) = match &guard.binding {
        WorkflowBinding::Action(id) => (
            "publication_retry",
            MessagePurpose::Publication,
            None,
            Some(id.as_str()),
        ),
        WorkflowBinding::Followup(id) => (
            "followup_dispatch",
            MessagePurpose::ThreadFollowup,
            Some(id.as_str()),
            None,
        ),
    };
    if guard.ready.model.is_empty()
        || guard.ready.model.len() > 128
        || guard.ready.effort.is_empty()
        || guard.ready.effort.len() > 64
        || batch.is_some_and(|id| id.is_empty() || id.len() > 512)
    {
        return Err(AppError::InvalidRequest.into());
    }
    let actor = Actor::local_operator();
    let target = validate_target(&tx.sql, &actor, command.target.target())?;
    if target != command.target {
        return Err(AppError::Conflict.into());
    }
    let input = SubmitMessage {
        target,
        submission_id: command.submission,
        text: command.body,
        purpose,
        report_ref: None,
    };
    let digest = workflow_digest(&guard, &input);
    if let Some((id, previous)) =
        find_receipt(&tx.sql, &actor_key(&actor), operation, &input.submission_id)?
    {
        if previous.as_slice() != digest {
            return Err(AppError::IdempotencyConflict.into());
        }
        return Ok(id);
    }
    if !current(&tx.sql, &input.target, &input.submission_id, &guard)? {
        return Err(AppError::StaleRevision.into());
    }
    let (id, _) = insert_intent(tx, &actor, &input, operation, &digest, None, batch)?;
    tx.sql.execute(
        "INSERT INTO workflow_dispatch(delivery_id,review_id,action_id,batch_id,settings_revision,head_sha,
              model,effort,latest_user_entry_id,latest_user_turn_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![id.as_str(),input.target.review_id().as_str(),action,batch,guard.settings.as_sqlite_integer(),
            guard.head.as_str(),guard.ready.model,guard.ready.effort,
            guard.ready.latest_user_entry_id.as_ref().map(|id|id.as_str()),
            guard.ready.latest_user_turn_id.as_ref().map(|id|id.as_str())],
    )?;
    emit_delivery(tx, input.target.target())?;
    Ok(id)
}

fn workflow_digest(guard: &WorkflowGuard, input: &SubmitMessage) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(request_digest(input));
    let binding = match &guard.binding {
        WorkflowBinding::Action(action) => action.as_str(),
        WorkflowBinding::Followup(batch) => batch,
    };
    for field in [
        binding,
        guard.head.as_str(),
        guard.ready.model.as_str(),
        guard.ready.effort.as_str(),
        guard
            .ready
            .latest_user_entry_id
            .as_ref()
            .map_or("", |id| id.as_str()),
        guard
            .ready
            .latest_user_turn_id
            .as_ref()
            .map_or("", |id| id.as_str()),
    ] {
        hash_field(&mut digest, field.as_bytes());
    }
    digest.update(guard.settings.get().to_be_bytes());
    digest.finalize().into()
}

pub(super) fn read_guard(
    db: &Connection,
    id: &DeliveryId,
) -> Result<Option<WorkflowGuard>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT action_id,batch_id,settings_revision,head_sha,model,effort,latest_user_entry_id,latest_user_turn_id
         FROM workflow_dispatch WHERE delivery_id=?1",
    )?;
    let mut rows = statement.query([id.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let binding = match (
        row.get::<_, Option<String>>(0)?,
        row.get::<_, Option<String>>(1)?,
    ) {
        (Some(action), None) => WorkflowBinding::Action(parse(&action)?),
        (None, Some(batch)) => WorkflowBinding::Followup(batch),
        _ => return Err(AppError::Internal.into()),
    };
    Ok(Some(WorkflowGuard {
        binding,
        settings: Revision::try_from(row.get::<_, i64>(2)?).map_err(|_| AppError::Internal)?,
        head: parse(text(row, 3)?)?,
        ready: ReadyWorkflow {
            model: row.get(4)?,
            effort: row.get(5)?,
            latest_user_entry_id: row
                .get::<_, Option<String>>(6)?
                .as_deref()
                .map(parse)
                .transpose()?,
            latest_user_turn_id: row
                .get::<_, Option<String>>(7)?
                .as_deref()
                .map(parse)
                .transpose()?,
        },
    }))
}

pub(super) fn current(
    db: &Connection,
    target: &LeadTarget,
    submission: &SubmissionId,
    guard: &WorkflowGuard,
) -> Result<bool, StoreError> {
    if settings_revision(db)? != guard.settings {
        return Ok(false);
    }
    let open: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM reviews r JOIN configured_projects p USING(origin,project_id)
         WHERE r.review_id=?1 AND r.head_sha=?2 AND r.state='open')
         AND NOT EXISTS(SELECT 1 FROM messages m JOIN outbox o ON o.delivery_id=m.message_id
             WHERE m.run_id=?3 AND m.session_key=?4 AND m.purpose IN ('user','report_feedback')
               AND m.state IN ('queued','dispatching','uncertain'))
         AND NOT EXISTS(SELECT 1 FROM sessions WHERE run_id=?3 AND (host_questions=1 OR host_activity='blocked'))",
        params![target.review_id().as_str(),guard.head.as_str(),
                target.run_id().as_str(),target.session_key().as_str()],|row|row.get(0),
    )?;
    if !open {
        return Ok(false);
    }
    match &guard.binding {
        WorkflowBinding::Action(action) => Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM action_heads h JOIN actions a USING(run_id,action_id)
             WHERE h.run_id=?1 AND a.action_id=?2 AND a.lead_session_key=?3
               AND a.pending_submission_id=?4 AND a.reviewed_sha=?5 AND a.request_entry_id=?6
               AND a.native_turn_id=?7 AND a.settings_revision=?8 AND a.authority_verified=1
               AND a.state='retry_wait')",
            params![target.run_id().as_str(),action.as_str(),target.session_key().as_str(),
                submission.as_str(),guard.head.as_str(),
                guard.ready.latest_user_entry_id.as_ref().map(|id|id.as_str()),
                guard.ready.latest_user_turn_id.as_ref().map(|id|id.as_str()),guard.settings.as_sqlite_integer()],
            |row|row.get(0),
        )?),
        WorkflowBinding::Followup(batch) => Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM followup_batches b JOIN followup_monitors m USING(review_id)
             LEFT JOIN report_heads h ON h.run_id=b.run_id
             WHERE b.batch_id=?1 AND b.review_id=?2 AND b.run_id=?3 AND b.lead_session_key=?4
               AND b.submission_id=?5 AND b.source_sha=?6 AND b.authority_verified=1 AND b.state='reserved'
               AND b.holds_slot=1 AND m.binding_verified=1 AND m.run_id=b.run_id AND m.generation=b.generation
               AND m.reason IS NULL AND (m.state='enabled' AND coalesce(m.policy_override,
                 (SELECT automatic FROM followup_preferences WHERE singleton=1),0)=1
                 OR b.manual_requested=1 AND m.state IN ('enabled','paused'))
               AND b.base_receipt_id IS h.receipt_id AND b.disposition_version=coalesce(h.disposition_version,0))",
            params![batch,target.review_id().as_str(),target.run_id().as_str(),target.session_key().as_str(),
                submission.as_str(),guard.head.as_str()],|row|row.get(0),
        )?),
    }
}

pub(super) fn advance(
    tx: &mut WriteTx<'_>,
    target: &LeadTarget,
    submission: &SubmissionId,
    guard: &WorkflowGuard,
    outcome: &DeliveryOutcome,
    timestamp: i64,
) -> Result<(), StoreError> {
    if matches!(outcome, DeliveryOutcome::Uncertain) {
        return uncertain(tx, target, submission, guard, timestamp);
    }
    let (turn, rejected) = match outcome {
        DeliveryOutcome::Started(input) => (Some(input.turn_id.as_str()), false),
        DeliveryOutcome::Rejected => (None, true),
        _ => return Ok(()),
    };
    let resource = match &guard.binding {
        WorkflowBinding::Followup(batch) => {
            let changed = if rejected {
                tx.sql.execute(
                    "UPDATE followup_batches SET state='reconciliation',holds_slot=0,
                      hold_reason='followup_native_delivery_rejected'
                     WHERE batch_id=?1 AND state='reserved' AND submission_id=?2 AND authority_verified=1",
                    params![batch,submission.as_str()],
                )?
            } else {
                tx.sql.execute(
                    "UPDATE followup_batches SET state='executing',native_turn_id=?3,hold_reason=NULL
                     WHERE batch_id=?1 AND state='reserved' AND submission_id=?2 AND authority_verified=1",
                    params![batch,submission.as_str(),turn],
                )?
            };
            (changed != 0).then_some(ResourceKey::Followup(Some(target.review_id().clone())))
        }
        WorkflowBinding::Action(action) => {
            let changed = tx.sql.execute(
                "UPDATE actions SET pending_submission_id=NULL,
                   state=CASE WHEN state IN ('cancelled','completed') THEN state WHEN ?3 IS NULL THEN 'needs_input' ELSE 'checking' END,
                   last_executed_submission_id=CASE WHEN ?3 IS NULL THEN last_executed_submission_id ELSE ?2 END,
                   last_executed_turn_id=coalesce(?3,last_executed_turn_id),
                   due_at_ms=CASE WHEN state IN ('cancelled','completed') THEN NULL ELSE ?4 END,
                   reason=CASE WHEN state IN ('cancelled','completed') THEN reason WHEN ?3 IS NULL THEN 'publication_delivery_rejected' ELSE NULL END,
                   revision=revision+1,updated_at_ms=max(updated_at_ms,?5)
                 WHERE action_id=?1 AND pending_submission_id=?2 AND authority_verified=1",
                params![action.as_str(),submission.as_str(),turn,timestamp.saturating_add(10_000),timestamp],
            )?;
            (changed != 0).then_some(ResourceKey::Action(Some(target.run_id().clone())))
        }
    };
    if let Some(resource) = resource {
        advance_queue(&tx.sql)?;
        tx.emit(&[
            resource,
            ResourceKey::Queue,
            ResourceKey::Review(Some(target.review_id().clone())),
        ])?;
    }
    Ok(())
}

fn uncertain(
    tx: &mut WriteTx<'_>,
    target: &LeadTarget,
    submission: &SubmissionId,
    guard: &WorkflowGuard,
    time: i64,
) -> Result<(), StoreError> {
    let resource = match &guard.binding {
        WorkflowBinding::Action(action) => {
            let changed=tx.sql.execute(
                "UPDATE actions SET state='needs_input',reason='publication_delivery_uncertain',due_at_ms=?3,
                    revision=revision+1,updated_at_ms=max(updated_at_ms,?4)
                 WHERE action_id=?1 AND pending_submission_id=?2 AND authority_verified=1
                   AND state NOT IN ('cancelled','completed') AND reason IS NOT 'publication_delivery_uncertain'",
                params![action.as_str(),submission.as_str(),time.saturating_add(60_000),time],
            )?;
            (changed != 0).then_some(ResourceKey::Action(Some(target.run_id().clone())))
        }
        WorkflowBinding::Followup(batch) => {
            let changed=tx.sql.execute(
                "UPDATE followup_batches SET hold_reason='followup_delivery_uncertain'
                 WHERE batch_id=?1 AND submission_id=?2 AND state='reserved' AND authority_verified=1
                   AND hold_reason IS NOT 'followup_delivery_uncertain'",
                params![batch,submission.as_str()],
            )?;
            (changed != 0).then_some(ResourceKey::Followup(Some(target.review_id().clone())))
        }
    };
    if let Some(resource) = resource {
        advance_queue(&tx.sql)?;
        tx.emit(&[resource, ResourceKey::Queue])?;
    }
    Ok(())
}
