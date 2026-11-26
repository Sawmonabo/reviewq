use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::hosts::HostPhase;
use reviewq_app::hosts::{
    ClaudeHeartbeat, ClaudeHeartbeatResult, ClaudeQuestionAddress, ClaudeStore,
};
use reviewq_domain::session::{HostProvider, LeadTarget, SessionTarget};
use rusqlite::{OptionalExtension, params};

use super::{activity_columns, allow_target};
use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::{lead_for_run, parse};
use crate::workspace::advance_queue;
use crate::{SqliteStore, StoreError};

mod child;
mod classic;
mod delivery;
mod host;
pub(crate) mod watch;
mod workflow;

impl SqliteStore {
    /// Stores each Claude companion that counts as gone by silence, or whose trusted host
    /// process is proven dead, as ended, as a companion that reports disconnecting is, and
    /// announces each change. Reads first, and opens a write only when one is to end. The
    /// runtime calls it at startup and every `WATCH_TICK`, and it reads the watch each time
    /// even with no companion stored, so a longer gap between readings means ReviewQ was not
    /// running.
    pub async fn end_companions(&self) -> Result<(), AppError> {
        self.companions.now();
        let ending = self
            .read(|db| {
                let mut statement = db.prepare_cached(&format!(
                    "SELECT s.run_id, c.session_key, c.generation,
                       reviewq_companion_gone(c.seen_at_ms), {}
                     FROM claude_companions c JOIN sessions s USING(session_key)
                     WHERE c.seen_at_ms<>0
                       AND (c.host_trusted=1 OR reviewq_companion_gone(c.seen_at_ms))",
                    host::COLUMNS
                ))?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                        host::trusted(row, 4)?,
                    ))
                })?;
                let mut ending = Vec::new();
                for row in rows {
                    let (run, session, generation, silent, host) = row?;
                    if silent || host.is_some_and(|host| host.is_dead()) {
                        ending.push((parse(&run)?, parse(&session)?, generation, silent));
                    }
                }
                Ok(ending)
            })
            .await
            .map_err(StoreError::app)?;
        if ending.is_empty() {
            return Ok(());
        }
        self.write(move |tx| {
            for (run, session, generation, silent) in &ending {
                classic::store_ended(tx, run, session, generation, *silent)?;
            }
            Ok(())
        })
        .await
        .map_err(StoreError::app)
    }
}

impl ClaudeStore for SqliteStore {
    fn fence_claude_question(
        &self,
        lead: LeadTarget,
        address: ClaudeQuestionAddress,
        invalidate: bool,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            let verify = move |db: &rusqlite::Connection| -> Result<LeadTarget, StoreError> {
                workflow::ensure_lead(db, &lead)?;
                let active: bool = db.prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM claude_companions c JOIN sessions s USING(session_key)
                     WHERE c.session_key=?1 AND c.generation=?2 AND c.turn_id=?3
                       AND reviewq_companion_live(c.seen_at_ms) AND s.host_activity IN ('working','blocked')
                       AND NOT EXISTS(SELECT 1 FROM claude_turns t WHERE t.session_key=c.session_key AND t.turn_id=c.turn_id))",
                )?.query_row(params![
                    lead.session_key().as_str(), address.generation, address.turn.as_str(),
                ], |row| row.get(0))?;
                if !active {
                    return Err(AppError::StaleRevision.into());
                }
                Ok(lead)
            };
            if invalidate {
                self.write(move |tx| {
                    let lead = verify(&tx.sql)?;
                    tx.emit(&[ResourceKey::Session(Some(lead.session_key().clone()))])?;
                    Ok(())
                })
                .await
                .map_err(StoreError::app)
            } else {
                self.read(move |db| verify(db).map(|_| ()))
                    .await
                    .map_err(StoreError::app)
            }
        })
    }

    fn claude_companion_available(
        &self,
        actor: Actor,
        target: SessionTarget,
    ) -> AppFuture<'_, Option<bool>> {
        Box::pin(async move {
            self.read(move |db| {
                let lead = lead_for_run(db, target.run_id())?.ok_or(AppError::NotFound)?;
                if lead.target() != &target {
                    return Err(AppError::NotFound.into());
                }
                allow_target(db, &actor, &lead)?;
                if lead.binding().identity().provider() != HostProvider::Claude {
                    return Ok(None);
                }
                // It tells the model the session is there, so it needs a live companion.
                let fresh = db
                    .prepare_cached(
                        "SELECT EXISTS(SELECT 1 FROM claude_companions WHERE session_key=?1
                       AND reviewq_companion_live(seen_at_ms))",
                    )?
                    .query_row([target.session_key().as_str()], |row| row.get(0))?;
                Ok(Some(fresh))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn pending_claude_launch(
        &self,
        context: reviewq_app::hosts::ClaudeContext,
    ) -> AppFuture<'_, bool> {
        Box::pin(async move {
            self.read(move |db| {
                let profile = context.profile.as_path().as_os_str().as_bytes();
                let reserved_profile = serde_json::to_string(profile).map_err(|_| AppError::Internal)?;
                // The provisional hint only drives the companion's bounded startup retries.
                // Linkage still requires the exact registered native identity and transcript.
                Ok(db.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM launch_attempts l JOIN reviews r USING(review_id)
                       JOIN configured_projects p ON p.origin=r.origin AND p.project_id=r.project_id
                       WHERE l.provider='claude' AND l.checkout=?2 AND l.created_at_ms>?3
                         AND l.native_id=?1 AND l.profile=?4 AND l.state IN ('pending','confirmed','uncertain')
                       UNION ALL
                       SELECT 1 FROM launch_attempts l JOIN reviews r USING(review_id)
                       JOIN configured_projects p ON p.origin=r.origin AND p.project_id=r.project_id
                       JOIN settings_repositories b ON b.project_id=r.project_id AND b.checkout=l.checkout AND b.common_dir=l.common_dir
                       WHERE l.provider='claude' AND l.checkout=?2 AND l.created_at_ms>?3
                         AND l.native_id IS NULL AND l.state='pending'
                         AND json_extract(l.invocation,'$.claude.profile')=?5)",
                    params![
                        context.session_id.as_str(),
                        context.cwd.as_path().as_os_str().as_bytes(),
                        now_ms()?.saturating_sub(30_000),
                        profile,
                        reserved_profile,
                    ],
                    |row| row.get(0),
                )?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn record_claude_classic(
        &self,
        lead: LeadTarget,
        event: reviewq_app::hosts::ClaudeClassicEvent,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| classic::record(tx, lead, event))
                .await
                .map_err(StoreError::app)
        })
    }

    fn record_claude_child(
        &self,
        lead: LeadTarget,
        child: reviewq_app::hosts::ProvenClaudeChild,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| child::record(tx, lead, child))
                .await
                .map_err(StoreError::app)
        })
    }

    fn offer_claude_delivery(
        &self,
        lead: LeadTarget,
        generation: String,
    ) -> AppFuture<'_, Option<reviewq_app::hosts::ClaudeOffer>> {
        Box::pin(async move {
            self.write(move |tx| delivery::offer(tx, lead, generation))
                .await
                .map_err(StoreError::app)
        })
    }

    fn claude_delivery(
        &self,
        lead: LeadTarget,
        offer: reviewq_app::hosts::ClaudeOffer,
    ) -> AppFuture<'_, reviewq_app::hosts::ClaudeSend> {
        Box::pin(async move {
            self.read(move |db| delivery::read(db, lead, &offer))
                .await
                .map_err(StoreError::app)
        })
    }

    fn authorize_claude_delivery(
        &self,
        lead: LeadTarget,
        offer: reviewq_app::hosts::ClaudeOffer,
        anchor: reviewq_app::hosts::ClaudeAnchor,
        checked_workflow: Option<reviewq_app::workflow::ReadyWorkflow>,
    ) -> AppFuture<'_, reviewq_app::hosts::DeliveryClaim> {
        Box::pin(async move {
            self.write(move |tx| delivery::authorize(tx, lead, offer, anchor, checked_workflow))
                .await
                .map_err(StoreError::app)
        })
    }

    fn replay_claude_delivery(
        &self,
        lead: LeadTarget,
        offer: reviewq_app::hosts::ClaudeOffer,
        outcome: reviewq_app::hosts::ClaudeDeliveryAck,
    ) -> AppFuture<'_, bool> {
        Box::pin(async move {
            self.read(move |db| delivery::replay(db, lead, offer, outcome))
                .await
                .map_err(StoreError::app)
        })
    }

    fn finish_claude_delivery(
        &self,
        lead: LeadTarget,
        offer: reviewq_app::hosts::ClaudeOffer,
        result: reviewq_app::hosts::ClaudeDeliveryResult,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| delivery::finish(tx, lead, offer, result))
                .await
                .map_err(StoreError::app)
        })
    }

    fn prove_claude_companion(
        &self,
        session: reviewq_domain::identity::SessionKey,
        proof: Option<[u8; 32]>,
        admit_gone: bool,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.read(move |db| {
                let (stored, gone, known): (Option<Vec<u8>>, Option<bool>, bool) = db
                    .prepare_cached(
                        "SELECT (SELECT proof_sha256 FROM claude_companions WHERE session_key=?1),
                           (SELECT reviewq_companion_gone(seen_at_ms) FROM claude_companions WHERE session_key=?1),
                           EXISTS(SELECT 1 FROM sessions WHERE session_key=?1)",
                    )?
                    .query_row([session.as_str()], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?;
                match stored {
                    Some(stored) if proof.is_some_and(|proof| stored == proof) => Ok(()),
                    Some(_) if admit_gone && gone == Some(true) => Ok(()),
                    None if admit_gone || !known => Ok(()),
                    _ => Err(AppError::Unauthorized.into()),
                }
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn record_claude_heartbeat(
        &self,
        lead: LeadTarget,
        observation: ClaudeHeartbeat,
    ) -> AppFuture<'_, Snapshot<ClaudeHeartbeatResult>> {
        Box::pin(async move {
            observation.validate()?;
            self.write(move |tx| heartbeat(tx, lead, observation))
                .await
                .map_err(StoreError::app)?
        })
    }
}

fn heartbeat(
    tx: &mut WriteTx<'_>,
    lead: LeadTarget,
    observation: ClaudeHeartbeat,
) -> Result<Result<Snapshot<ClaudeHeartbeatResult>, AppError>, StoreError> {
    let db = &tx.sql;
    let native = lead.binding().identity();
    if native.provider() != HostProvider::Claude
        || lead_for_run(db, lead.run_id())?.as_ref() != Some(&lead)
    {
        return Err(AppError::NotFound.into());
    }
    allow_target(db, &Actor::native_session(native.clone()), &lead)?;
    let key = lead.session_key().as_str();
    let now = now_ms()?;
    // A stored companion whose trusted host is proven dead ends now. A report of its own
    // generation, as from a helper that outlived the process, is refused; another generation
    // takes the session over at once.
    if let Some(generation) = host::dead_generation(db, key)? {
        classic::store_ended(tx, lead.run_id(), lead.session_key(), &generation, false)?;
        if generation == observation.generation {
            return Ok(Err(AppError::StaleRevision));
        }
    }
    let db = &tx.sql;
    // Whether the queue showed the companion, and whether this report binds a permission
    // prompt that arrived while the companion was unknown: to the turn it names when that turn
    // is still running, whatever its generation, since a newer generation that takes over
    // mid-turn was refused while the stored one was unknown and its prompt landed there. A
    // report with no turn or an ended one drops the prompt: a newer generation's first report
    // names none, so a prompt of the process it replaced is dropped.
    let turn = observation.turn.as_ref().map(|value| value.as_str());
    let (was_connected, binds): (bool, bool) = db
        .prepare_cached(
            "SELECT NOT reviewq_companion_gone(seen_at_ms), permission_unbound=1 AND ?2 IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM claude_turns WHERE session_key=?1 AND turn_id=?2)
             FROM claude_companions WHERE session_key=?1",
        )?
        .query_row(params![key, turn], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?
        .unwrap_or((false, false));
    let (host_pid, host_start, host_boot, host_namespace) = host::params(observation.host.as_ref());
    let recorded = db.execute(
        &format!("INSERT INTO claude_companions(session_key,generation,sequence,seen_at_ms,turn_id,latest_prompt_id,model,effort,latest_prompt_turn_id,latest_prompt_manual,turn_step,
           host_pid,host_start,host_boot,host_namespace,host_since_ms,proof_sha256)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,CASE WHEN ?12 IS NULL THEN NULL ELSE ?4 END,?17)
         ON CONFLICT(session_key) DO UPDATE SET
            {},
            generation=excluded.generation, sequence=excluded.sequence, seen_at_ms=excluded.seen_at_ms,
            proof_sha256=excluded.proof_sha256,
            permission_turn_id=CASE
              WHEN ?16 THEN excluded.turn_id
              WHEN generation=excluded.generation AND turn_id IS excluded.turn_id
                AND coalesce(excluded.turn_step,-1)<=coalesce(permission_step,-1)
              THEN permission_turn_id ELSE NULL END,
            permission_step=CASE WHEN ?16 THEN excluded.turn_step ELSE permission_step END,
            permission_unbound=0,
            turn_step=excluded.turn_step,
            turn_id=excluded.turn_id, latest_prompt_id=excluded.latest_prompt_id,
            model=excluded.model,effort=excluded.effort,
            latest_prompt_turn_id=excluded.latest_prompt_turn_id,latest_prompt_manual=excluded.latest_prompt_manual
         WHERE (generation=excluded.generation AND proof_sha256=excluded.proof_sha256 AND sequence<excluded.sequence) OR
               (generation<>excluded.generation AND reviewq_companion_gone(seen_at_ms))", host::upsert()),
        params![key, observation.generation, observation.sequence as i64, now,
            observation.turn.as_ref().map(|value| value.as_str()),
            observation.latest_prompt.as_ref().map(|value| value.as_str()),
            observation.model, observation.effort,
            observation.latest_prompt_turn.as_ref().map(|value| value.as_str()),
            observation.latest_prompt_manual, observation.step.map(|value| value as i64),
            host_pid, host_start, host_boot, host_namespace, binds, observation.proof.as_slice()],
    )?;
    if recorded == 0 {
        return Err(AppError::StaleRevision.into());
    }
    let mut visible = !was_connected;
    let mut attention = Vec::new();
    if binds && let Some(turn) = &observation.turn {
        attention.extend(classic::permission_alert(db, &lead, turn.to_string())?);
    }
    if let Some(activity) = observation.activity {
        let permission: bool = db.query_row(
            "SELECT coalesce(permission_turn_id=turn_id,0) FROM claude_companions WHERE session_key=?1",
            [key], |row| row.get(0),
        )?;
        let (phase, questions) = if permission {
            (Some("blocked"), true)
        } else {
            activity_columns(Some(activity))
        };
        let kind = if activity.has_questions && observation.turn.is_some() {
            observation.question_id.as_ref().map(|question| {
                (
                    reviewq_app::notifications::Kind::AgentQuestion,
                    question.to_string(),
                )
            })
        } else if activity.phase == HostPhase::Failed {
            if let Some(turn) = &observation.turn {
                let failed: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM claude_turns WHERE session_key=?1 AND turn_id=?2 AND companion_generation=?3 AND normal=0)",
                    params![key,turn.as_str(),observation.generation], |row|row.get(0),
                )?;
                failed.then(|| {
                    (
                        reviewq_app::notifications::Kind::AgentFailure,
                        turn.to_string(),
                    )
                })
            } else {
                None
            }
        } else {
            None
        };
        if let Some((kind, native_ref)) = kind
            && let Some(id) = crate::notifications::native::insert(
                db,
                HostProvider::Claude,
                lead.target(),
                reviewq_app::notifications::NativeOccurrence {
                    kind,
                    native_ref,
                    at_ms: Some(now),
                    desktop_episode: None,
                },
            )?
        {
            attention.push(id);
        }
        visible |= crate::hosts::record_activity(db, key, phase, questions)?;
    }
    if visible {
        advance_queue(db)?;
        tx.emit(&[
            ResourceKey::Queue,
            ResourceKey::Run(Some(lead.run_id().clone())),
            ResourceKey::Review(Some(lead.review_id().clone())),
            ResourceKey::Session(Some(lead.session_key().clone())),
        ])?;
    }
    crate::notifications::publish(tx, &attention)?;
    let label = tx.sql.query_row(
        "SELECT project_path || ' · !' || iid FROM reviews WHERE review_id=?1",
        [lead.review_id().as_str()],
        |row| row.get(0),
    )?;
    Ok(Ok(snapshot(&tx.sql, ClaudeHeartbeatResult { label })?))
}
