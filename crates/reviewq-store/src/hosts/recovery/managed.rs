use reviewq_app::access::{Actor, AppError};
use reviewq_app::hosts::recovery::{InterruptClaim, InterruptState, ManagedInterruptTarget};
use reviewq_domain::identity::{NativeTurnId, RunId, SessionKey};
use reviewq_domain::session::{HostProvider, LeadTarget};
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;
use crate::connection::{WriteTx, read_head};
use crate::events::now_ms;
use crate::hosts::claude::COMPANION_FRESH_MS;
use crate::queue::{lead_for_run, parse};

use super::{cancel_interrupted, emit};

pub(super) fn target(
    db: &Connection,
    actor: &Actor,
    run: &RunId,
    session: &SessionKey,
) -> Result<Option<ManagedInterruptTarget>, StoreError> {
    let lead = scoped(db, actor, run, session)?;
    match managed_job(db, &lead).map_err(StoreError::app) {
        Ok(None) => Ok(Some(ManagedInterruptTarget::Codex(lead))),
        Ok(Some(_)) => Ok(fresh_claude_turn(db, &lead)?.map(ManagedInterruptTarget::Claude)),
        Err(AppError::Forbidden) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn claim(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    run: RunId,
    session: SessionKey,
    turn: NativeTurnId,
) -> Result<InterruptClaim, StoreError> {
    let db = &tx.sql;
    let target = scoped(db, &actor, &run, &session)?;
    let job = managed_job(db, &target)?;
    let prior: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT state,native_job_id FROM managed_interrupts
         WHERE run_id=?1 AND lead_session_key=?2 AND native_turn_id=?3",
            params![run.as_str(), session.as_str(), turn.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let state = prior
        .as_ref()
        .map(|(state, _)| parse::<InterruptState>(state))
        .transpose()?;
    if prior.is_some_and(|(_, prior_job)| prior_job != job) {
        return Err(AppError::Conflict.into());
    }
    let acquired = state.is_none_or(|state| state == InterruptState::Deferred);
    let state = if acquired {
        if job.is_some() {
            active_claude_turn(db, &target, &turn)?;
        }
        db.execute(
            "INSERT INTO managed_interrupts(run_id,lead_session_key,native_turn_id,native_job_id,state,updated_at_ms)
             VALUES(?1,?2,?3,?4,'reserved',?5)
             ON CONFLICT(run_id,lead_session_key,native_turn_id)
               DO UPDATE SET state='reserved',updated_at_ms=excluded.updated_at_ms",
            params![run.as_str(), session.as_str(), turn.as_str(), job, now_ms()?],
        )?;
        InterruptState::Reserved
    } else {
        state.ok_or(AppError::Internal)?
    };
    if job.is_none() {
        cancel_interrupted(db, &target, &turn)?;
    }
    if acquired {
        emit(tx, &target)?;
    }
    Ok(InterruptClaim {
        target,
        claude_job: job,
        turn,
        state,
        acquired,
        cursor: read_head(&tx.sql)?,
    })
}

pub(super) fn check(db: &Connection, claim: &InterruptClaim) -> Result<(), StoreError> {
    let current = scoped(
        db,
        &Actor::local_operator(),
        claim.target.run_id(),
        claim.target.session_key(),
    )?;
    if current != claim.target || managed_job(db, &current)? != claim.claude_job || !claim.acquired
    {
        return Err(AppError::Conflict.into());
    }
    let reserved: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM managed_interrupts WHERE run_id=?1 AND lead_session_key=?2
         AND native_turn_id=?3 AND native_job_id IS ?4 AND state='reserved')",
        params![
            current.run_id().as_str(),
            current.session_key().as_str(),
            claim.turn.as_str(),
            claim.claude_job
        ],
        |row| row.get(0),
    )?;
    if !reserved {
        return Err(AppError::Conflict.into());
    }
    if claim.claude_job.is_some() {
        active_claude_turn(db, &current, &claim.turn)?;
    }
    Ok(())
}

fn scoped(
    db: &Connection,
    actor: &Actor,
    run: &RunId,
    session: &SessionKey,
) -> Result<LeadTarget, StoreError> {
    let target = lead_for_run(db, run)?.ok_or(AppError::NotFound)?;
    if target.session_key() != session {
        return Err(AppError::NotFound.into());
    }
    super::super::allow_target(db, actor, &target)?;
    Ok(target)
}

fn managed_job(db: &Connection, target: &LeadTarget) -> Result<Option<String>, StoreError> {
    let address = target.binding().identity();
    let (count, distinct_jobs, job): (i64, i64, Option<String>) = db.query_row(
        "SELECT count(*),count(DISTINCT native_job_id),min(native_job_id) FROM launch_attempts
         WHERE run_id=?1 AND session_key=?2 AND provider=?3 AND native_id=?4
           AND state IN ('pending','confirmed','uncertain')",
        params![
            target.run_id().as_str(),
            target.session_key().as_str(),
            crate::workspace::provider_label(address.provider()),
            address.native_id().as_str()
        ],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    match (address.provider(), count, distinct_jobs, job) {
        (HostProvider::Codex, count, 0, None) if count > 0 => Ok(None),
        (HostProvider::Claude, _, 1, Some(job))
            if !job.is_empty()
                && job.len() <= 64
                && job.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            Ok(Some(job))
        }
        _ => Err(AppError::Forbidden.into()),
    }
}

fn active_claude_turn(
    db: &Connection,
    target: &LeadTarget,
    turn: &NativeTurnId,
) -> Result<(), StoreError> {
    if fresh_claude_turn(db, target)?.as_ref() == Some(turn) {
        Ok(())
    } else {
        Err(AppError::Conflict.into())
    }
}

fn fresh_claude_turn(
    db: &Connection,
    target: &LeadTarget,
) -> Result<Option<NativeTurnId>, StoreError> {
    let turn: Option<String> = db
        .query_row(
            "SELECT c.turn_id FROM claude_companions c JOIN sessions s USING(session_key)
         WHERE s.run_id=?1 AND c.session_key=?2 AND c.turn_id IS NOT NULL AND c.seen_at_ms>?3
           AND s.host_activity IN ('working','blocked')
           AND NOT EXISTS(SELECT 1 FROM claude_turns t
             WHERE t.session_key=c.session_key AND t.turn_id=c.turn_id)",
            params![
                target.run_id().as_str(),
                target.session_key().as_str(),
                now_ms()?.saturating_sub(COMPANION_FRESH_MS)
            ],
            |row| row.get(0),
        )
        .optional()?;
    turn.as_deref().map(parse).transpose()
}
