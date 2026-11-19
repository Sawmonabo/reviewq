use reviewq_app::access::AppError;
use reviewq_app::hosts::recovery::{PreparedRecovery, RecoveryAuthorization, RecoveryKind};
use reviewq_app::hosts::{
    DeliveryClaim, DeliveryOutcome, DeliveryWork, MessageBody, MessagePurpose,
};
use reviewq_app::workflow::{
    WorkflowAdmission, WorkflowBinding, WorkflowRequirement, WorkflowRequirements,
};
use reviewq_domain::identity::{MrIdentity, ReportHash, SubmissionId};
use reviewq_domain::session::LeadTarget;
use rusqlite::{Connection, params};
use uuid::Uuid;

use super::{IN_SCOPE, emit, preferences};
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::hosts::admit_workflow;
use crate::queue::{lead_for_run, parse};
use crate::workspace::settings_revision;

const READY: &str = "
   NOT EXISTS(SELECT 1 FROM messages m JOIN outbox o ON o.delivery_id=m.message_id
      WHERE m.run_id=e.run_id AND m.state IN ('queued','dispatching','uncertain')
        AND (e.delivery_id IS NULL OR m.message_id<>e.delivery_id))
   AND NOT EXISTS(SELECT 1 FROM sessions s WHERE s.run_id=e.run_id
        AND (s.host_questions=1 OR s.host_activity IN ('blocked','working')))
   AND NOT EXISTS(SELECT 1 FROM report_handoffs h WHERE h.run_id=e.run_id
       AND (h.state='pending_completion' OR h.turn_id=e.failed_turn_id AND h.state='recorded'))
   AND NOT EXISTS(SELECT 1 FROM action_heads h JOIN actions a USING(run_id,action_id)
       WHERE h.run_id=e.run_id AND a.state IN ('working','checking','retry_wait'))
   AND NOT EXISTS(SELECT 1 FROM followup_batches b WHERE b.run_id=e.run_id AND b.authority_verified=1
       AND (b.state='reserved' OR b.state='executing' AND (
          coalesce(b.continuation_turn_id,b.native_turn_id)<>e.failed_turn_id OR
          NOT EXISTS(SELECT 1 FROM followup_monitors monitor WHERE monitor.review_id=b.review_id
             AND monitor.run_id=b.run_id AND monitor.generation=b.generation AND monitor.state='enabled'
             AND monitor.binding_verified=1 AND coalesce(monitor.policy_override,
                (SELECT automatic FROM followup_preferences WHERE singleton=1),0)=1)
       )))";

pub(super) fn reserve(tx: &mut WriteTx<'_>, input: PreparedRecovery) -> Result<(), StoreError> {
    let PreparedRecovery {
        candidate,
        authorization,
        ready,
        reviewer,
    } = input;
    let db = &tx.sql;
    let lead = &candidate.lead;
    if !preferences(db)?.automatic
        || settings_revision(db)? != candidate.settings
        || lead_for_run(db, lead.run_id())?.as_ref() != Some(lead)
        || authorization.episode != candidate.episode.id
        || authorization.failed_turn != candidate.episode.turn
        || authorization.kind != candidate.episode.kind
        || candidate.account.as_ref() != Some(&authorization.account)
        || authorization.kind != RecoveryKind::ModelCapacity
            && candidate.prior_capacity.as_ref() == Some(&authorization.capacity)
    {
        return Err(AppError::StaleRevision.into());
    }
    let occupied: i64 = db.query_row(
        "SELECT count(*) FROM recovery_episodes WHERE state IN ('queued','uncertain')",
        [],
        |row| row.get(0),
    )?;
    if occupied >= 2 {
        return Err(AppError::Busy.into());
    }
    let now = now_ms()?;
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM recovery_episodes e JOIN reviews r USING(review_id)
           WHERE e.episode_id=?1 AND e.run_id=?2 AND e.lead_session_key=?3 AND e.revision=?4
             AND e.state='waiting' AND e.due_at_ms<=?5 AND e.head_sha=?6 AND ({IN_SCOPE}) AND ({READY}))"
    );
    let eligible: bool = db.query_row(
        &sql,
        params![
            candidate.episode.id,
            lead.run_id().as_str(),
            lead.session_key().as_str(),
            candidate.episode.revision.as_sqlite_integer(),
            now,
            candidate.head.as_str()
        ],
        |row| row.get(0),
    )?;
    if !eligible {
        return Err(AppError::StaleRevision.into());
    }
    let submission: SubmissionId = parse(&Uuid::new_v4().to_string())?;
    db.execute(
        "UPDATE recovery_episodes SET state='queued',revision=revision+1,reason=NULL,due_at_ms=NULL,
          submission_id=?2,capacity_hash=?3,model_provider=?4,root_turn_id=?5,model=?6,effort=?7,reviewer_id=?8,
          submitted_entry_id=?9
         WHERE episode_id=?1",
        params![candidate.episode.id,submission.as_str(),authorization.capacity.as_str(),authorization.model_provider,
            authorization.root_turn.as_str(),ready.model,ready.effort,reviewer.to_string(),
            ready.latest_user_entry_id.as_ref().map(|item|item.as_str())],
    )?;
    let delivery = admit_workflow(
        tx,
        WorkflowAdmission {
            target: lead.clone(),
            submission,
            body: MessageBody::new(
                "Resume the interrupted private ReviewQ review using the previously established scope. Continue the unfinished analysis and complete the report handoff. Preserve unrelated findings and user decisions. Do not publish or edit notes, approve, merge, resolve discussions, or change Jira without a new explicit user request.",
            )?,
            binding: WorkflowBinding::Recovery(authorization),
            settings: candidate.settings,
            head: candidate.head,
            ready,
        },
    )?;
    tx.sql.execute(
        "UPDATE recovery_episodes SET delivery_id=?2 WHERE episode_id=?1",
        params![candidate.episode.id, delivery.as_str()],
    )?;
    emit(tx, lead)
}

pub(crate) fn current(
    db: &Connection,
    lead: &LeadTarget,
    submission: &SubmissionId,
    auth: &RecoveryAuthorization,
) -> Result<bool, StoreError> {
    if !preferences(db)?.automatic {
        return Ok(false);
    }
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM recovery_episodes e JOIN reviews r USING(review_id)
           WHERE e.episode_id=?1 AND e.review_id=?2 AND e.run_id=?3 AND e.lead_session_key=?4
             AND e.submission_id=?5 AND e.failed_turn_id=?6 AND e.state='queued' AND ({IN_SCOPE}) AND ({READY}))"
    );
    Ok(db.query_row(
        &sql,
        params![
            auth.episode,
            lead.review_id().as_str(),
            lead.run_id().as_str(),
            lead.session_key().as_str(),
            submission.as_str(),
            auth.failed_turn.as_str()
        ],
        |row| row.get(0),
    )?)
}

pub(crate) fn read_authorization(
    db: &Connection,
    id: &str,
) -> Result<RecoveryAuthorization, StoreError> {
    let (turn,root,kind,account,capacity,provider)=db.query_row(
        "SELECT failed_turn_id,root_turn_id,kind,account_hash,capacity_hash,model_provider FROM recovery_episodes WHERE episode_id=?1",
        [id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,
            row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?)),
    )?;
    Ok(RecoveryAuthorization {
        episode: id.to_owned(),
        failed_turn: parse(&turn)?,
        root_turn: parse(&root)?,
        kind: parse(&kind)?,
        account: parse(&account)?,
        capacity: parse(&capacity)?,
        model_provider: provider,
    })
}

pub(crate) fn requirements(
    db: &Connection,
    identity: MrIdentity,
    auth: &RecoveryAuthorization,
) -> Result<WorkflowRequirements, StoreError> {
    let reviewer: String = db.query_row(
        "SELECT reviewer_id FROM recovery_episodes WHERE episode_id=?1",
        [&auth.episode],
        |row| row.get(0),
    )?;
    Ok(WorkflowRequirements {
        identity,
        reviewer_id: reviewer.parse().map_err(|_| AppError::Internal)?,
        requirement: WorkflowRequirement::Recovery,
    })
}

pub(crate) fn refresh_allowance(
    db: &Connection,
    claim: &DeliveryClaim,
    hash: &ReportHash,
) -> Result<(), StoreError> {
    let DeliveryWork::Dispatch {
        workflow: Some(guard),
        ..
    } = &claim.work
    else {
        return Err(AppError::Conflict.into());
    };
    let WorkflowBinding::Recovery(auth) = &guard.binding else {
        return Err(AppError::Conflict.into());
    };
    if claim.purpose != MessagePurpose::CodexRecovery {
        return Err(AppError::Conflict.into());
    }
    db.execute(
        "UPDATE recovery_episodes AS e SET capacity_hash=?3 WHERE episode_id=?1 AND delivery_id=?2
           AND state='queued' AND capacity_hash=?4 AND revision=?5 AND resumed_turn_id IS NULL
           AND (kind='model_capacity' OR NOT EXISTS(SELECT 1 FROM recovery_episodes p WHERE p.run_id=e.run_id
                AND p.lead_session_key=e.lead_session_key AND p.kind=e.kind AND p.capacity_hash=?3 AND p.resumed_turn_id IS NOT NULL))",
        params![auth.episode,claim.delivery_id.as_str(),hash.as_str(),auth.capacity.as_str(),guard.authority_revision.as_sqlite_integer()],
    )?;
    Ok(())
}

pub(crate) fn advance(
    tx: &mut WriteTx<'_>,
    lead: &LeadTarget,
    submission: &SubmissionId,
    auth: &RecoveryAuthorization,
    result: &DeliveryOutcome,
    at: i64,
) -> Result<(), StoreError> {
    let (state, reason, turn) = match result {
        DeliveryOutcome::Started(input) => ("started", None, Some(input.turn_id.as_str())),
        DeliveryOutcome::Uncertain => ("uncertain", Some("recovery_delivery_uncertain"), None),
        DeliveryOutcome::Rejected => ("cancelled", Some("recovery_dispatch_rejected"), None),
        _ => return Ok(()),
    };
    let changed=tx.sql.execute(
        "UPDATE recovery_episodes SET state=CASE WHEN state IN ('cancelled','disabled','complete') THEN state ELSE ?3 END,
            reason=CASE WHEN state IN ('cancelled','disabled','complete') THEN reason ELSE ?4 END,
            resumed_turn_id=coalesce(resumed_turn_id,?5),due_at_ms=NULL,revision=revision+1,
            attempted_count=attempted_count+CASE WHEN ?5 IS NOT NULL AND resumed_turn_id IS NULL THEN 1 ELSE 0 END,
            last_attempt_at_ms=max(coalesce(last_attempt_at_ms,0),?6)
         WHERE episode_id=?1 AND submission_id=?2 AND (state IS NOT ?3 OR reason IS NOT ?4 OR resumed_turn_id IS NOT ?5)",
        params![auth.episode,submission.as_str(),state,reason,turn,at],
    )?;
    if let Some(turn) = turn {
        let adopted=tx.sql.execute(
            "UPDATE followup_batches AS b SET continuation_turn_id=?4,holds_slot=1,hold_reason=NULL
             WHERE review_id=?1 AND run_id=?2 AND lead_session_key=?3 AND state='executing'
               AND authority_verified=1 AND coalesce(continuation_turn_id,native_turn_id)=?5
               AND EXISTS(SELECT 1 FROM recovery_episodes e WHERE e.episode_id=?6 AND e.state='started'
                 AND e.submission_id=?7 AND e.resumed_turn_id=?4)
               AND EXISTS(SELECT 1 FROM followup_monitors m WHERE m.review_id=b.review_id AND m.run_id=b.run_id
                 AND m.generation=b.generation AND m.state='enabled' AND m.binding_verified=1
                 AND coalesce(m.policy_override,(SELECT automatic FROM followup_preferences WHERE singleton=1),0)=1)",
            params![lead.review_id().as_str(),lead.run_id().as_str(),lead.session_key().as_str(),turn,auth.failed_turn.as_str(),
                auth.episode,submission.as_str()],
        )?;
        if adopted != 0 {
            tx.emit(&[reviewq_app::events::ResourceKey::Followup(Some(
                lead.review_id().clone(),
            ))])?;
        }
    }
    if changed != 0 {
        emit(tx, lead)?;
    }
    Ok(())
}
