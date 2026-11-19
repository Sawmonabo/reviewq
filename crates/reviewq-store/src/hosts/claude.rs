use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::hosts::{ClaudeHeartbeat, ClaudeHeartbeatResult, ClaudeStore};
use reviewq_domain::session::{HostProvider, LeadTarget, SessionTarget};
use rusqlite::params;

use super::{activity_columns, allow_target};
use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::lead_for_run;
use crate::workspace::advance_queue;
use crate::{SqliteStore, StoreError};

pub(super) const COMPANION_FRESH_MS: i64 = 30_000;

mod child;
mod classic;
mod delivery;
mod workflow;

impl ClaudeStore for SqliteStore {
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
                let fresh = db.prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM claude_companions WHERE session_key=?1 AND seen_at_ms>?2)",
                )?.query_row(
                    params![target.session_key().as_str(),now_ms()?.saturating_sub(COMPANION_FRESH_MS)],
                    |row| row.get(0),
                )?;
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
                Ok(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM launch_attempts l JOIN reviews r USING(review_id)
                     JOIN configured_projects p ON p.origin=r.origin AND p.project_id=r.project_id
                     WHERE l.id=?1 AND l.provider='claude' AND l.checkout=?2
                       AND l.state IN ('pending','confirmed') AND l.created_at_ms>?3
                       AND (l.profile IS NULL OR l.profile=?4))",
                    params![
                        context.session_id.as_str(),
                        context.cwd.as_path().as_os_str().as_bytes(),
                        now_ms()?.saturating_sub(30_000),
                        context.profile.as_path().as_os_str().as_bytes()
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

    fn record_claude_heartbeat(
        &self,
        lead: LeadTarget,
        observation: ClaudeHeartbeat,
    ) -> AppFuture<'_, Snapshot<ClaudeHeartbeatResult>> {
        Box::pin(async move {
            observation.validate()?;
            self.write(move |tx| heartbeat(tx, lead, observation))
                .await
                .map_err(StoreError::app)
        })
    }
}

fn heartbeat(
    tx: &mut WriteTx<'_>,
    lead: LeadTarget,
    observation: ClaudeHeartbeat,
) -> Result<Snapshot<ClaudeHeartbeatResult>, StoreError> {
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
    let recorded = db.execute(
        "INSERT INTO claude_companions(session_key,generation,sequence,seen_at_ms,turn_id,latest_prompt_id,model,effort,latest_prompt_turn_id,latest_prompt_manual,turn_step)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(session_key) DO UPDATE SET
            generation=excluded.generation, sequence=excluded.sequence, seen_at_ms=excluded.seen_at_ms,
            permission_turn_id=CASE WHEN generation=excluded.generation AND turn_id IS excluded.turn_id
              AND coalesce(excluded.turn_step,-1)<=coalesce(permission_step,-1)
              THEN permission_turn_id ELSE NULL END,
            turn_step=excluded.turn_step,
            turn_id=excluded.turn_id, latest_prompt_id=excluded.latest_prompt_id,
            model=excluded.model,effort=excluded.effort,
            latest_prompt_turn_id=excluded.latest_prompt_turn_id,latest_prompt_manual=excluded.latest_prompt_manual
         WHERE (generation=excluded.generation AND sequence<excluded.sequence) OR
               (generation<>excluded.generation AND seen_at_ms<=?12)",
        params![key, observation.generation, observation.sequence as i64, now,
            observation.turn.as_ref().map(|value| value.as_str()),
            observation.latest_prompt.as_ref().map(|value| value.as_str()),
            observation.model, observation.effort,
            observation.latest_prompt_turn.as_ref().map(|value| value.as_str()),
            observation.latest_prompt_manual, observation.step.map(|value| value as i64), now.saturating_sub(COMPANION_FRESH_MS)],
    )?;
    if recorded == 0 {
        return Err(AppError::StaleRevision.into());
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
        let changed = db.execute(
            "UPDATE sessions SET host_activity=?2, host_questions=?3
             WHERE session_key=?1 AND (host_activity,host_questions) IS DISTINCT FROM (?2,?3)",
            params![key, phase, questions],
        )?;
        if changed != 0 {
            advance_queue(db)?;
            tx.emit(&[
                ResourceKey::Queue,
                ResourceKey::Run(Some(lead.run_id().clone())),
                ResourceKey::Review(Some(lead.review_id().clone())),
                ResourceKey::Session(Some(lead.session_key().clone())),
            ])?;
        }
    }
    let label = tx.sql.query_row(
        "SELECT project_path || ' · !' || iid FROM reviews WHERE review_id=?1",
        [lead.review_id().as_str()],
        |row| row.get(0),
    )?;
    snapshot(&tx.sql, ClaudeHeartbeatResult { label })
}
