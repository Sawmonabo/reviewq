use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::actions::{
    Action, ActionCommandStore, ActionDeclaration, ActionWork, Approval, Evidence, Intent,
    PublicationEvidence, PublicationRetry, State, confirms_prior_target,
};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_app::queue::MergeRequestState;
use reviewq_app::sources::WorkflowSnapshot;
use reviewq_app::workflow::{NativeCall, WorkflowAdmission, WorkflowBinding};
use reviewq_domain::identity::{MrIdentity, ReportHash, Revision, SubmissionId};
use reviewq_domain::session::LeadTarget;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::{by_id, current};
use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::hosts::admit_workflow;
use crate::hosts::claude::watch;
use crate::notifications::insert_notification;
use crate::queue::{identity_for_review, lead_for_run, parse};
use crate::workspace::{advance_queue, configured, settings_revision};
use crate::{SqliteStore, StoreError};

impl ActionCommandStore for SqliteStore {
    fn run_lead(&self, session: reviewq_domain::identity::SessionKey) -> AppFuture<'_, LeadTarget> {
        Box::pin(async move {
            self.read(move |db| crate::reports::authority::run_lead(db, &session))
                .await
                .map_err(StoreError::app)
        })
    }

    fn action_source(&self, lead: LeadTarget) -> AppFuture<'_, (MrIdentity, Revision)> {
        Box::pin(async move {
            self.read(move |db| {
                let identity = verified_lead(db, &lead)?;
                Ok((identity, settings_revision(db)?))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn declared_action(
        &self,
        call: NativeCall,
        digest: ReportHash,
    ) -> AppFuture<'_, Option<Snapshot<Action>>> {
        Box::pin(async move {
            self.read(move |db| {
                verified_lead(db, &call.lead)?;
                replay(db, &call, &digest)?
                    .map(|value| snapshot(db, value))
                    .transpose()
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn declare_action(
        &self,
        call: NativeCall,
        input: ActionDeclaration,
        digest: ReportHash,
        observed: WorkflowSnapshot,
        settings: Revision,
    ) -> AppFuture<'_, Snapshot<Action>> {
        Box::pin(async move {
            input.validate()?;
            self.write(move |tx| declare(tx, call, input, digest, observed, settings))
                .await
                .map_err(StoreError::app)
        })
    }

    fn current_action_work(&self, lead: LeadTarget) -> AppFuture<'_, Option<ActionWork>> {
        Box::pin(async move {
            self.read(move |db| work_for_lead(db, lead))
                .await
                .map_err(StoreError::app)
        })
    }

    fn record_action_evidence(
        &self,
        work: ActionWork,
        evidence: PublicationEvidence,
        superseded_request: bool,
    ) -> AppFuture<'_, Snapshot<Action>> {
        Box::pin(async move {
            self.write(move |tx| record_evidence(tx, work, evidence, superseded_request, None))
                .await
                .map_err(StoreError::app)
        })
    }
}

pub(super) fn work_for_lead(
    db: &Connection,
    lead: LeadTarget,
) -> Result<Option<ActionWork>, StoreError> {
    let identity = verified_lead(db, &lead)?;
    let Some(action) = current(db, lead.run_id())? else {
        return Ok(None);
    };
    if !action.authority_verified {
        return Ok(None);
    }
    let (actor, settings, declared_at_ms) = db.query_row(
                    "SELECT authenticated_actor_id, settings_revision, created_at_ms FROM actions WHERE action_id=?1",
                    [action.id.as_str()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get(2)?)),
                )?;
    Ok(Some(ActionWork {
        target: lead,
        identity,
        action,
        authenticated_actor: actor.parse().map_err(|_| AppError::Internal)?,
        settings: Revision::try_from(settings).map_err(|_| AppError::Internal)?,
        declared_at_ms,
    }))
}

fn verified_lead(db: &Connection, lead: &LeadTarget) -> Result<MrIdentity, StoreError> {
    if lead_for_run(db, lead.run_id())?.as_ref() != Some(lead) {
        return Err(AppError::NotFound.into());
    }
    let identity = identity_for_review(db, lead.review_id())?;
    if !configured(db, &identity)? {
        return Err(AppError::Unavailable.into());
    }
    Ok(identity)
}

fn replay(
    db: &Connection,
    call: &NativeCall,
    digest: &ReportHash,
) -> Result<Option<Action>, StoreError> {
    let previous = db.query_row(
        "SELECT action_id, declaration_sha256, request_entry_id, source_generation, source_position
         FROM actions WHERE run_id=?1 AND lead_session_key=?2 AND native_turn_id=?3 AND native_call_id=?4
            AND authority_verified=1",
        params![call.lead.run_id().as_str(), call.lead.session_key().as_str(), call.turn_id.as_str(), call.call_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                  row.get::<_, String>(3)?, row.get::<_, i64>(4)?)),
    ).optional()?;
    let Some((id, expected, input, generation, position)) = previous else {
        return Ok(None);
    };
    if expected != digest.as_str()
        || call.request_entry_id.as_ref().map(|id| id.as_str()) != Some(input.as_str())
        || call.source_generation != generation
        || i64::try_from(call.source_position).ok() != Some(position)
    {
        return Err(AppError::IdempotencyConflict.into());
    }
    by_id(db, call.lead.run_id(), &id)
}

fn declare(
    tx: &mut WriteTx<'_>,
    call: NativeCall,
    input: ActionDeclaration,
    digest: ReportHash,
    observed: WorkflowSnapshot,
    settings: Revision,
) -> Result<Snapshot<Action>, StoreError> {
    let db = &tx.sql;
    let identity = verified_lead(db, &call.lead)?;
    if let Some(previous) = replay(db, &call, &digest)? {
        return snapshot(db, previous);
    }
    if call.source_generation.is_empty()
        || call.source_generation.len() > 512
        || call.call_id.is_empty()
        || call.call_id.len() > 512
    {
        return Err(AppError::InvalidRequest.into());
    }
    let request = call.request_entry_id.as_ref().ok_or(AppError::Conflict)?;
    if settings != settings_revision(db)?
        || observed.review.identity != identity
        || observed.authenticated_actor == 0
        || observed.review.head_sha != input.reviewed_sha
        || observed.review.state != MergeRequestState::Open
        || !at_open_head(db, &call.lead, input.reviewed_sha.as_str())?
    {
        return Err(AppError::StaleRevision.into());
    }
    let run = call.lead.run_id().as_str();
    let session = call.lead.session_key().as_str();
    if let Some(hold) = crate::hosts::unresolved_workflow_input(db, &call.lead, &call.turn_id)? {
        return Err(hold.into());
    }
    let invalid: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE run_id=?1 AND session_key=?2 AND actor_key IS NOT NULL
           AND ((purpose IN ('user','report_feedback') AND state IN ('queued','dispatching','uncertain'))
             OR (native_turn_id=?3 AND native_input_id=?4 AND purpose NOT IN ('user','report_feedback'))))",
        params![run, session, call.turn_id.as_str(), request.as_str()], |row| row.get(0),
    )?;
    if invalid {
        return Err(AppError::Conflict.into());
    }
    let position = i64::try_from(call.source_position).map_err(|_| AppError::InvalidRequest)?;
    // A declaration answers a later input than the head's, or the same one: calls under one
    // input share its position, and the later to arrive here replaces the head.
    let current_order: Option<(String, i64, String)> = db.query_row(
        "SELECT a.source_generation,a.source_position,a.request_entry_id FROM action_heads h JOIN actions a USING(run_id,action_id)
         WHERE h.run_id=?1 AND a.authority_verified=1",
        [run], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    if current_order.is_some_and(|(generation, prior, input)| {
        generation != call.source_generation
            || prior > position
            || (prior == position && input != request.as_str())
    }) {
        return Err(AppError::Conflict.into());
    }
    let id = Uuid::new_v4().to_string();
    let time = now_ms()?;
    db.execute(
        "UPDATE actions SET state='cancelled', reason='new_native_declaration', due_at_ms=NULL,
            revision=revision+1, updated_at_ms=max(updated_at_ms,?2)
         WHERE action_id=(SELECT action_id FROM action_heads WHERE run_id=?1)
           AND authority_verified=1 AND state NOT IN ('completed','cancelled')",
        params![run, time],
    )?;
    let attention = db.query_row(
        "SELECT v.attention_receipt_id FROM report_heads h JOIN report_versions v USING(run_id,receipt_id)
         WHERE h.run_id=?1",
        [run], |row| row.get::<_, String>(0),
    ).optional()?;
    let state = if input.intent == Intent::Cancel {
        State::Cancelled
    } else {
        State::Working
    };
    let approval = if matches!(input.intent, Intent::Approval | Intent::Both) {
        Approval::Unknown
    } else {
        Approval::NotRequested
    };
    db.execute(
        "INSERT INTO actions(action_id,run_id,lead_session_key,native_turn_id,native_call_id,request_entry_id,
            declaration_sha256,reviewed_sha,authenticated_actor_id,attention_receipt_id,intent,state,
            approval_state,attempts,revision,authority_verified,due_at_ms,created_at_ms,updated_at_ms,
            source_generation,source_position,settings_revision)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,0,1,1,?14,?15,?15,?16,?17,?18)",
        params![
            id, run, session, call.turn_id.as_str(), call.call_id, request.as_str(), digest.as_str(),
            input.reviewed_sha.as_str(), observed.authenticated_actor.to_string(), attention,
            input.intent.to_string(), state.to_string(), approval.to_string(),
            (state != State::Cancelled).then_some(time.saturating_add(10_000)), time,
            call.source_generation, position, settings.as_sqlite_integer(),
        ],
    )?;
    let mut prior = db.prepare_cached(
        "SELECT t.note_id,t.observed_at_ms FROM action_targets t JOIN actions a USING(action_id)
         WHERE a.run_id=?1 AND a.reviewed_sha=?2 AND a.authenticated_actor_id=?3 AND a.authority_verified=1
           AND t.body_sha256=?4 AND t.discussion_id IS ?5 AND t.evidence_state='confirmed'
           AND t.note_id IS NOT NULL ORDER BY t.observed_at_ms DESC LIMIT 1",
    )?;
    for (index, target) in input.targets.iter().enumerate() {
        let known = prior
            .query_row(
                params![
                    run,
                    input.reviewed_sha.as_str(),
                    observed.authenticated_actor.to_string(),
                    target.hash.as_str(),
                    target.discussion
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?
            .filter(|(note, _)| {
                note.parse::<u64>().ok().is_some_and(|id| {
                    confirms_prior_target(&observed, target, id, &input.reviewed_sha)
                })
            });
        let (note, time) = known.map_or((None, None), |(id, time)| (Some(id), time));
        db.prepare_cached(
            "INSERT INTO action_targets(action_id,ordinal,body_sha256,discussion_id,evidence_state,note_id,observed_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
        )?.execute(params![id, (index + 1) as i64, target.hash.as_str(), target.discussion,
            if note.is_some() {"confirmed"} else {"unknown"},note,time])?;
    }
    drop(prior);
    db.execute(
        "INSERT INTO action_heads(run_id,action_id) VALUES(?1,?2)
         ON CONFLICT(run_id) DO UPDATE SET action_id=excluded.action_id",
        params![run, id],
    )?;
    emit(tx, &call.lead, None)?;
    let action = by_id(&tx.sql, call.lead.run_id(), &id)?.ok_or(AppError::Internal)?;
    snapshot(&tx.sql, action)
}

fn at_open_head(db: &Connection, target: &LeadTarget, head: &str) -> Result<bool, StoreError> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM reviews WHERE review_id=?1 AND head_sha=?2 AND state='open')",
        params![target.review_id().as_str(), head],
        |row| row.get(0),
    )?)
}

/// Whether `lead`'s Claude companion is neither live nor gone.
fn companion_unknown(db: &Connection, lead: &LeadTarget) -> Result<bool, StoreError> {
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM claude_companions WHERE session_key=?1 AND {})",
        watch::unknown("seen_at_ms")
    );
    Ok(db
        .prepare_cached(&sql)?
        .query_row([lead.session_key().as_str()], |row| row.get(0))?)
}

pub(super) fn record_evidence(
    tx: &mut WriteTx<'_>,
    work: ActionWork,
    evidence: PublicationEvidence,
    superseded: bool,
    retry: Option<PublicationRetry>,
) -> Result<Snapshot<Action>, StoreError> {
    let db = &tx.sql;
    if verified_lead(db, &work.target)? != work.identity
        || evidence.actor != work.authenticated_actor
    {
        return Err(AppError::Conflict.into());
    }
    let action = current(db, work.target.run_id())?.ok_or(AppError::NotFound)?;
    if action.id != work.action.id
        || action.revision != work.action.revision
        || evidence.targets.len() != action.targets.len()
    {
        return Err(AppError::StaleRevision.into());
    }
    if matches!(action.state, State::Completed | State::Cancelled) {
        return snapshot(db, action);
    }
    let id = action.id.as_str();
    let checked = evidence.checked_at_ms;
    let revision = action
        .revision
        .checked_add(1)
        .ok_or(AppError::Internal)?
        .as_sqlite_integer();
    let valid_head = work.settings == settings_revision(db)?
        && action.reviewed_sha.as_ref() == Some(&evidence.auth_head)
        && evidence.open
        && at_open_head(db, &work.target, evidence.auth_head.as_str())?;
    let retry_allowed = retry.as_ref().is_some_and(|candidate| {
        action.attempts < 3
            && action.pending_submission.is_none()
            && candidate.ready.latest_user_entry_id == action.request_entry
            && candidate.ready.latest_user_turn_id == action.turn
            && evidence.approval != Approval::Unknown
            && evidence
                .targets
                .iter()
                .all(|target| target.state != Evidence::Unknown)
    });
    let (state, reason, notify) = if !valid_head {
        (
            State::Cancelled,
            Some("publication_head_or_scope_changed"),
            false,
        )
    } else {
        for (index, observed) in evidence.targets.iter().enumerate() {
            let ordinal = (index + 1) as i64;
            db.prepare_cached(
                "UPDATE action_targets SET evidence_state=?3,note_id=?4,observed_at_ms=?5
                 WHERE action_id=?1 AND ordinal=?2 AND evidence_state<>'confirmed'",
            )?
            .execute(params![
                id,
                ordinal,
                observed.state.to_string(),
                observed.note_id,
                checked
            ])?;
            record_check(
                db,
                id,
                revision,
                Some(ordinal),
                &observed.state.to_string(),
                observed.note_id.as_deref(),
                &evidence,
            )?;
        }
        if evidence.approval != Approval::NotRequested {
            record_check(
                db,
                id,
                revision,
                None,
                &evidence.approval.to_string(),
                None,
                &evidence,
            )?;
            db.execute(
                "UPDATE actions SET approval_state=?2,approval_observed_at_ms=?3
                 WHERE action_id=?1 AND (approval_state<>'confirmed' OR ?2<>'unknown')",
                params![id, evidence.approval.to_string(), checked],
            )?;
        }
        let complete = evidence
            .targets
            .iter()
            .all(|target| target.state == Evidence::Confirmed)
            && (matches!(
                evidence.approval,
                Approval::Confirmed | Approval::NotRequested
            ));
        if complete {
            (State::Completed, None, false)
        } else if superseded {
            (State::Cancelled, Some("new_native_input"), false)
        } else if retry_allowed {
            (State::RetryWait, None, false)
        } else {
            (
                State::NeedsInput,
                Some("publication_outcome_unconfirmed"),
                action.state != State::NeedsInput,
            )
        }
    };
    // Asking the reviewer acts on the lead being absent, so it waits while the lead's Claude
    // companion is neither live nor gone; the publication stays due and is checked again.
    if state == State::NeedsInput && companion_unknown(db, &work.target)? {
        return Err(AppError::Busy.into());
    }
    let unknown = evidence.approval == Approval::Unknown
        || evidence
            .targets
            .iter()
            .any(|target| target.state == Evidence::Unknown);
    let uncertainty: u8 = db.query_row(
        "SELECT uncertainty_checks FROM actions WHERE action_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let next_uncertainty = if state == State::NeedsInput && unknown {
        uncertainty.saturating_add(1).min(4)
    } else {
        0
    };
    let next = if state == State::NeedsInput {
        Some(checked.saturating_add(if unknown {
            10_000_i64 << uncertainty.min(3)
        } else {
            60_000
        }))
    } else {
        None
    };
    let submission: Option<SubmissionId> = if state == State::RetryWait {
        Some(parse(&Uuid::new_v4().to_string())?)
    } else {
        None
    };
    db.execute(
        "UPDATE actions SET state=?2,reason=?3,revision=?4,checked_at_ms=?5,updated_at_ms=max(updated_at_ms,?5),
            due_at_ms=?6,uncertainty_checks=?7,
            pending_submission_id=coalesce(?8,pending_submission_id),attempts=attempts+CASE WHEN ?8 IS NULL THEN 0 ELSE 1 END
         WHERE action_id=?1",
        params![id,state.to_string(),reason,revision,checked,next,next_uncertainty,
            submission.as_ref().map(|id|id.as_str())],
    )?;
    if let (Some(submission), Some(retry)) = (submission, retry) {
        admit_workflow(
            tx,
            WorkflowAdmission {
                target: work.target.clone(),
                submission,
                body: retry.prompt,
                binding: WorkflowBinding::Action(action.id.clone()),
                settings: work.settings,
                head: evidence.auth_head,
                ready: retry.ready,
            },
        )?;
    }
    emit(tx, &work.target, notify.then_some(id))?;
    let result = by_id(&tx.sql, work.target.run_id(), id)?.ok_or(AppError::Internal)?;
    snapshot(&tx.sql, result)
}

fn record_check(
    db: &Connection,
    id: &str,
    revision: i64,
    target: Option<i64>,
    state: &str,
    note: Option<&str>,
    evidence: &PublicationEvidence,
) -> Result<(), StoreError> {
    db.prepare_cached(
        "INSERT INTO action_evidence(action_id,check_revision,target_ordinal,state,note_id,
            actor_id,head_sha,observed_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
    )?
    .execute(params![
        id,
        revision,
        target,
        state,
        note,
        evidence.actor.to_string(),
        evidence.auth_head.as_str(),
        evidence.checked_at_ms
    ])?;
    Ok(())
}

fn emit(tx: &mut WriteTx<'_>, target: &LeadTarget, notify: Option<&str>) -> Result<(), StoreError> {
    let mut keys = vec![
        ResourceKey::Action(Some(target.run_id().clone())),
        ResourceKey::Run(Some(target.run_id().clone())),
        ResourceKey::Review(Some(target.review_id().clone())),
        ResourceKey::Queue,
    ];
    if let Some(action) = notify
        && let Some(notification) = insert_notification(
            &tx.sql,
            Observation {
                source: Source::Action,
                occurrence_id: action.to_owned(),
                kind: Kind::ActionNeedsInput,
                review: target.review_id().clone(),
                run: Some(target.run_id().clone()),
                session: Some(target.session_key().clone()),
                event_ref: Some(action.to_owned()),
                label: "Publication outcome needs verification".to_owned(),
                occurred_at_ms: now_ms()?,
                desktop_episode: None,
            },
        )?
    {
        keys.push(ResourceKey::Notification(Some(notification)));
    }
    advance_queue(&tx.sql)?;
    tx.emit(&keys)?;
    Ok(())
}
