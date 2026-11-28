use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::recovery::{
    Candidate, Episode, InterruptClaim, InterruptState, NativeFailure, Preferences,
    PreparedRecovery, RecoveryStore,
};
use reviewq_domain::identity::{NativeTurnId, Revision, RunId, SessionKey};
use reviewq_domain::session::{HostProvider, LeadTarget};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::{identity_for_review, lead_for_run, parse};
use crate::workspace::{advance_queue, settings_revision};
use crate::{SqliteStore, StoreError};

mod dispatch;
mod managed;
pub(crate) use dispatch::{advance, current, read_authorization, refresh_allowance, requirements};

pub(super) const IN_SCOPE:&str="
   e.settings_revision=(SELECT settings_revision FROM service_state WHERE singleton=1)
   AND r.state='open' AND r.head_sha=e.head_sha
   AND EXISTS(SELECT 1 FROM configured_projects p WHERE p.origin=r.origin AND p.project_id=r.project_id)
   AND NOT EXISTS(SELECT 1 FROM runs newer JOIN runs original ON original.run_id=e.run_id
      WHERE newer.review_id=e.review_id AND (newer.started_at_ms,newer.run_id)>(original.started_at_ms,original.run_id))";

impl RecoveryStore for SqliteStore {
    fn recovery_preferences(&self, actor: Actor) -> AppFuture<'_, Snapshot<Preferences>> {
        Box::pin(async move {
            operator(&actor)?;
            self.read(|db| snapshot(db, preferences(db)?))
                .await
                .map_err(StoreError::app)
        })
    }

    fn save_recovery_preferences(
        &self,
        actor: Actor,
        expected: Revision,
        automatic: bool,
    ) -> AppFuture<'_, Snapshot<Preferences>> {
        Box::pin(async move {
            operator(&actor)?;
            self.write(move |tx| set_preferences(tx, expected, automatic))
                .await
                .map_err(StoreError::app)
        })
    }

    fn recovery_state(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
    ) -> AppFuture<'_, Snapshot<Option<Episode>>> {
        Box::pin(async move {
            self.read(move |db| {
                let lead = target(db, &actor, &run, &session)?;
                snapshot(db, latest(db, &lead)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn cancel_recovery(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
        episode_id: String,
        expected: Revision,
    ) -> AppFuture<'_, Snapshot<Episode>> {
        Box::pin(async move {
            operator(&actor)?;
            self.write(move |tx| {
                let lead=target(&tx.sql,&actor,&run,&session)?;
                let current=latest(&tx.sql,&lead)?.ok_or(AppError::NotFound)?;
                if current.id!=episode_id || current.revision!=expected {return Err(AppError::StaleRevision.into());}
                if matches!(current.state,reviewq_app::hosts::recovery::RecoveryState::Complete | reviewq_app::hosts::recovery::RecoveryState::Cancelled) {
                    return snapshot(&tx.sql,current);
                }
                let changed=tx.sql.execute(
                    "UPDATE recovery_episodes SET state='cancelled',reason='recovery_cancelled_by_operator',
                        due_at_ms=NULL,revision=revision+1
                     WHERE episode_id=?1 AND run_id=?2 AND lead_session_key=?3 AND revision=?4",
                    params![episode_id,run.as_str(),session.as_str(),expected.as_sqlite_integer()],
                )?;
                if changed!=1 {return Err(AppError::StaleRevision.into());}
                emit(tx,&lead)?;
                snapshot(&tx.sql,latest(&tx.sql,&lead)?.ok_or(AppError::NotFound)?)
            }).await.map_err(StoreError::app)
        })
    }

    fn due_recovery(&self) -> AppFuture<'_, Vec<Candidate>> {
        Box::pin(async move {
            self.write(|tx| {
                retire_obsolete(tx)?;
                due(&tx.sql)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn defer_recovery(
        &self,
        candidate: Candidate,
        reason: &'static str,
        cancelled: bool,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                let changed=tx.sql.execute(
                    "UPDATE recovery_episodes SET state=CASE WHEN ?3 THEN 'cancelled' ELSE state END,
                        reason=?4,due_at_ms=CASE WHEN ?3 THEN NULL ELSE ?5 END,
                        revision=revision+(reason IS NOT ?4 OR ?3)
                     WHERE episode_id=?1 AND revision=?2 AND state='waiting'",
                     params![candidate.episode.id,candidate.episode.revision.as_sqlite_integer(),
                        cancelled,reason,now_ms()?.saturating_add(30_000)],
                )?;
                if changed != 0 {
                    if cancelled || candidate.episode.reason.as_deref() != Some(reason) {
                        emit(tx, &candidate.lead)?;
                    } else {
                        tx.emit(&[ResourceKey::Session(Some(candidate.lead.session_key().clone()))])?;
                    }
                }
                Ok(())
            }).await.map_err(StoreError::app)
        })
    }

    fn reserve_recovery(&self, input: PreparedRecovery) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| dispatch::reserve(tx, input))
                .await
                .map_err(StoreError::app)
        })
    }

    fn interrupt_target(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
    ) -> AppFuture<'_, Snapshot<Option<reviewq_app::hosts::recovery::ManagedInterruptTarget>>> {
        Box::pin(async move {
            operator(&actor)?;
            self.read(move |db| snapshot(db, managed::target(db, &actor, &run, &session)?))
                .await
                .map_err(StoreError::app)
        })
    }

    fn claim_interrupt(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
        turn: NativeTurnId,
    ) -> AppFuture<'_, InterruptClaim> {
        Box::pin(async move {
            operator(&actor)?;
            self.write(move |tx| managed::claim(tx, actor, run, session, turn))
                .await
                .map_err(StoreError::app)
        })
    }

    fn check_interrupt(&self, claim: InterruptClaim) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.read(move |db| managed::check(db, &claim))
                .await
                .map_err(StoreError::app)
        })
    }

    fn finish_interrupt(
        &self,
        claim: InterruptClaim,
        state: InterruptState,
    ) -> AppFuture<'_, Snapshot<InterruptState>> {
        Box::pin(async move {
            self.write(move |tx| {
                if state==InterruptState::Reserved || !claim.acquired && state!=InterruptState::Interrupted {
                    return Err(AppError::Conflict.into());
                }
                let changed = tx.sql.execute(
                    "UPDATE managed_interrupts SET state=?4,updated_at_ms=?5 WHERE run_id=?1
                     AND lead_session_key=?2 AND native_turn_id=?3 AND state IN ('reserved','uncertain')
                     AND native_job_id IS ?6",
                    params![
                        claim.target.run_id().as_str(),
                        claim.target.session_key().as_str(),
                        claim.turn.as_str(),
                        state.to_string(),
                        now_ms()?,
                        claim.claude_job,
                    ],
                )?;
                if changed!=1 {
                    let current:String=tx.sql.query_row(
                        "SELECT state FROM managed_interrupts WHERE run_id=?1 AND lead_session_key=?2 AND native_turn_id=?3",
                        params![claim.target.run_id().as_str(),claim.target.session_key().as_str(),claim.turn.as_str()],|row|row.get(0),
                    )?;
                    if current!=state.to_string() {return Err(AppError::Conflict.into());}
                } else {
                    emit(tx,&claim.target)?;
                }
                snapshot(&tx.sql, state)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn operator(actor: &Actor) -> Result<(), AppError> {
    if !matches!(actor.scope(), ActorScope::LocalOperator) {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

fn target(
    db: &Connection,
    actor: &Actor,
    run: &RunId,
    session: &SessionKey,
) -> Result<LeadTarget, StoreError> {
    let lead = lead_for_run(db, run)?.ok_or(AppError::NotFound)?;
    if lead.session_key() != session || lead.binding().identity().provider() != HostProvider::Codex
    {
        return Err(AppError::NotFound.into());
    }
    super::allow_target(db, actor, &lead)?;
    Ok(lead)
}

fn preferences(db: &Connection) -> Result<Preferences, StoreError> {
    let (revision, automatic) = db
        .query_row(
            "SELECT revision,automatic FROM recovery_preferences WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or((1, false));
    Ok(Preferences {
        revision: revision_for(revision)?,
        automatic,
    })
}

fn set_preferences(
    tx: &mut WriteTx<'_>,
    expected: Revision,
    automatic: bool,
) -> Result<Snapshot<Preferences>, StoreError> {
    let db = &tx.sql;
    let old = preferences(db)?;
    if old.revision != expected {
        return Err(AppError::StaleRevision.into());
    }
    if old.automatic == automatic {
        return snapshot(db, old);
    }
    let now = now_ms()?;
    let next = expected.checked_add(1).ok_or(AppError::Internal)?;
    db.execute(
        "INSERT INTO recovery_preferences VALUES(1,?1,?2)
         ON CONFLICT(singleton) DO UPDATE SET revision=excluded.revision,automatic=excluded.automatic",
        params![next.as_sqlite_integer(),automatic],
    )?;
    if automatic {
        db.execute(
            "UPDATE recovery_episodes SET state='waiting',reason=NULL,due_at_ms=?1,revision=revision+1
             WHERE state='disabled' AND delivery_id IS NULL", [now],
        )?;
    } else {
        db.execute(
            "UPDATE recovery_episodes SET state='disabled',reason='recovery_disabled',due_at_ms=NULL,revision=revision+1
             WHERE state IN ('waiting','queued')", [],
        )?;
    }
    tx.delivery_changed = true;
    advance_queue(&tx.sql)?;
    tx.emit(&[
        ResourceKey::Settings,
        ResourceKey::Queue,
        ResourceKey::Run(None),
    ])?;
    snapshot(
        &tx.sql,
        Preferences {
            revision: next,
            automatic,
        },
    )
}

pub(crate) fn observe_failure(
    tx: &mut WriteTx<'_>,
    session: &HistorySession,
    failure: NativeFailure,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    let lease = &session.target;
    let Some(lead) = lead_for_run(db, lease.run_id())? else {
        return Ok(());
    };
    if lead.target() != lease || lead.binding() != &session.binding {
        return Ok(());
    }
    let existing:bool=db.query_row(
        "SELECT EXISTS(SELECT 1 FROM recovery_episodes WHERE run_id=?1 AND lead_session_key=?2 AND failed_turn_id=?3)",
        params![lead.run_id().as_str(),lead.session_key().as_str(),failure.turn.as_str()],|row|row.get(0),
    )?;
    if existing {
        return Ok(());
    }
    let observed = now_ms()?;
    let Some(timing) = failure.at_ms else {
        return Ok(());
    };
    let active: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1 AND started_at_ms<=?2)",
        params![lead.run_id().as_str(), timing],
        |row| row.get(0),
    )?;
    if !active {
        return Ok(());
    }
    let stopped:bool=db.query_row(
        "SELECT EXISTS(SELECT 1 FROM recovery_episodes e LEFT JOIN outbox o ON o.delivery_id=e.delivery_id
           WHERE e.run_id=?1 AND e.lead_session_key=?2 AND e.state='cancelled'
             AND e.reason IN ('recovery_cancelled_by_operator','recovery_cancelled_by_interrupt','recovery_superseded_by_user_message')
             AND (e.resumed_turn_id=?3 OR o.uncertain_turn_id=?3))",
         params![lead.run_id().as_str(),lead.session_key().as_str(),failure.turn.as_str()],|row|row.get(0),
    )?;
    db.execute(
        "UPDATE recovery_episodes SET state=CASE WHEN resumed_turn_id=?3 THEN 'complete' ELSE 'cancelled' END,
            reason=CASE WHEN resumed_turn_id=?3 THEN 'recovery_resumed_turn_failed' ELSE 'recovery_native_turn_changed' END,
            revision=revision+1,due_at_ms=NULL WHERE run_id=?1 AND lead_session_key=?2 AND state IN ('waiting','disabled','started')",
        params![lead.run_id().as_str(),lead.session_key().as_str(),failure.turn.as_str()],
    )?;
    let automatic = preferences(db)?.automatic;
    let head: String = db.query_row(
        "SELECT head_sha FROM reviews WHERE review_id=?1",
        [lead.review_id().as_str()],
        |row| row.get(0),
    )?;
    let attempts: i64 = db.query_row(
        "SELECT coalesce(max(attempted_count),0) FROM recovery_episodes
         WHERE run_id=?1 AND lead_session_key=?2 AND resumed_turn_id=?3 AND kind=?4",
        params![
            lead.run_id().as_str(),
            lead.session_key().as_str(),
            failure.turn.as_str(),
            failure.kind.to_string()
        ],
        |row| row.get(0),
    )?;
    let wait = 30_000_i64.saturating_mul(1 << attempts.min(4)).min(300_000);
    // A closing call accepted in the failed turn ended its recovery eligibility.
    let closed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ledger_calls WHERE run_id=?1 AND tool='submit_review'
           AND native_turn_id=?2)",
        params![lead.run_id().as_str(), failure.turn.as_str()],
        |row| row.get(0),
    )?;
    let (state, reason, due) = if closed {
        ("complete", None, None)
    } else if stopped {
        ("cancelled", Some("prior_recovery_cancelled"), None)
    } else if automatic {
        ("waiting", None, Some(observed.saturating_add(wait)))
    } else {
        ("disabled", Some("recovery_disabled"), None)
    };
    db.execute(
        "INSERT INTO recovery_episodes(episode_id,review_id,run_id,lead_session_key,failed_turn_id,kind,state,
         revision,head_sha,settings_revision,attempted_count,observed_at_ms,due_at_ms,reason,account_hash,model_provider)
         VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![Uuid::new_v4().to_string(),lead.review_id().as_str(),lead.run_id().as_str(),lead.session_key().as_str(),
            failure.turn.as_str(),failure.kind.to_string(),state,
            head,settings_revision(db)?.as_sqlite_integer(),attempts,observed,due,
            reason,failure.account.as_ref().map(|hash|hash.as_str()),failure.model_provider],
    )?;
    emit(tx, &lead)?;
    Ok(())
}

pub(crate) fn complete_turn(
    tx: &mut WriteTx<'_>,
    lead: &LeadTarget,
    turn: &NativeTurnId,
) -> Result<(), StoreError> {
    let changed=tx.sql.execute(
        "UPDATE recovery_episodes SET state='complete',reason=NULL,due_at_ms=NULL,revision=revision+1
         WHERE run_id=?1 AND lead_session_key=?2 AND (failed_turn_id=?3 OR resumed_turn_id=?3)
           AND state IN ('waiting','disabled','queued','started')",
        params![lead.run_id().as_str(),lead.session_key().as_str(),turn.as_str()],
    )?;
    if changed != 0 {
        emit(tx, lead)?;
    }
    Ok(())
}

fn cancel_interrupted(
    db: &Connection,
    lead: &LeadTarget,
    turn: &NativeTurnId,
) -> Result<(), StoreError> {
    db.execute(
        "UPDATE recovery_episodes SET state='cancelled',reason='recovery_cancelled_by_interrupt',due_at_ms=NULL,revision=revision+1
         WHERE run_id=?1 AND lead_session_key=?2 AND (failed_turn_id=?3 OR resumed_turn_id=?3)
           AND state IN ('waiting','disabled','queued','started')",
        params![lead.run_id().as_str(),lead.session_key().as_str(),turn.as_str()],
    )?;
    Ok(())
}

pub(crate) fn cancel(db: &Connection, lead: &LeadTarget, reason: &str) -> Result<(), StoreError> {
    db.execute(
        "UPDATE recovery_episodes SET state='cancelled',reason=?3,due_at_ms=NULL,revision=revision+1
         WHERE run_id=?1 AND lead_session_key=?2 AND state IN ('waiting','disabled','queued','started','uncertain')",
        params![lead.run_id().as_str(),lead.session_key().as_str(),reason],
    )?;
    Ok(())
}

fn latest(db: &Connection, lead: &LeadTarget) -> Result<Option<Episode>, StoreError> {
    let mut query=db.prepare_cached(
        "SELECT episode_id,revision,failed_turn_id,kind,state,reason,due_at_ms FROM recovery_episodes
         WHERE run_id=?1 AND lead_session_key=?2 ORDER BY observed_at_ms DESC,episode_id DESC LIMIT 1"
    )?;
    let mut rows = query.query(params![lead.run_id().as_str(), lead.session_key().as_str()])?;
    rows.next()?.map(|row| decode(row, 6)).transpose()
}

fn retire_obsolete(tx: &mut WriteTx<'_>) -> Result<(), StoreError> {
    let sql = format!(
        "UPDATE recovery_episodes SET state='cancelled',reason='recovery_binding_changed',
             due_at_ms=NULL,revision=revision+1
         WHERE episode_id IN (
            SELECT e.episode_id FROM recovery_episodes e JOIN reviews r USING(review_id)
             WHERE e.state IN ('waiting','disabled','queued') AND NOT ({IN_SCOPE})
             ORDER BY e.observed_at_ms,e.episode_id LIMIT 32)",
    );
    if tx.sql.execute(&sql, [])? > 0 {
        tx.delivery_changed = true;
        advance_queue(&tx.sql)?;
        tx.emit(&[ResourceKey::Queue, ResourceKey::Run(None)])?;
    }
    Ok(())
}

fn due(db: &Connection) -> Result<Vec<Candidate>, StoreError> {
    if !preferences(db)?.automatic {
        return Ok(Vec::new());
    }
    let sql=format!(
        "SELECT e.episode_id,e.revision,e.failed_turn_id,e.kind,e.state,e.reason,e.run_id,e.lead_session_key,
            e.head_sha,e.settings_revision,e.account_hash,
            (SELECT p.capacity_hash FROM recovery_episodes p WHERE p.run_id=e.run_id
               AND p.lead_session_key=e.lead_session_key AND p.kind=e.kind AND p.attempted_count>0
               AND p.resumed_turn_id IS NOT NULL AND p.capacity_hash IS NOT NULL
               ORDER BY p.observed_at_ms DESC,p.episode_id DESC LIMIT 1),e.model_provider,e.due_at_ms
         FROM recovery_episodes e JOIN reviews r USING(review_id)
         WHERE e.state='waiting' AND e.due_at_ms<=?1 AND ({IN_SCOPE})
         ORDER BY e.due_at_ms,e.episode_id LIMIT 2"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query([now_ms()?])?;
    let mut output = Vec::new();
    while let Some(row) = rows.next()? {
        let run: RunId = parse(&row.get::<_, String>(6)?)?;
        let Some(lead) = lead_for_run(db, &run)? else {
            continue;
        };
        if lead.session_key().as_str() != row.get::<_, String>(7)? {
            continue;
        }
        output.push(Candidate {
            episode: decode(row, 13)?,
            identity: identity_for_review(db, lead.review_id())?,
            lead,
            head: parse(&row.get::<_, String>(8)?)?,
            settings: revision_for(row.get(9)?)?,
            account: row
                .get::<_, Option<String>>(10)?
                .as_deref()
                .map(parse)
                .transpose()?,
            prior_capacity: row
                .get::<_, Option<String>>(11)?
                .as_deref()
                .map(parse)
                .transpose()?,
            model_provider: row.get(12)?,
        });
    }
    Ok(output)
}

fn decode(row: &Row<'_>, due_column: usize) -> Result<Episode, StoreError> {
    Ok(Episode {
        id: row.get(0)?,
        revision: revision_for(row.get(1)?)?,
        turn: parse(&row.get::<_, String>(2)?)?,
        kind: parse(&row.get::<_, String>(3)?)?,
        state: parse(&row.get::<_, String>(4)?)?,
        reason: row.get(5)?,
        due_at_ms: row.get(due_column)?,
    })
}

fn revision_for(value: i64) -> Result<Revision, StoreError> {
    Revision::try_from(value).map_err(|_| AppError::Internal.into())
}

fn emit(tx: &mut WriteTx<'_>, lead: &LeadTarget) -> Result<(), StoreError> {
    advance_queue(&tx.sql)?;
    tx.emit(&[
        ResourceKey::Queue,
        ResourceKey::Run(Some(lead.run_id().clone())),
        ResourceKey::Review(Some(lead.review_id().clone())),
    ])?;
    Ok(())
}
