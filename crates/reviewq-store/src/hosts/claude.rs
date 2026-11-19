use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::hosts::{ClaudeHeartbeat, ClaudeHeartbeatResult, ClaudeStore};
use reviewq_domain::session::{HostProvider, LeadTarget};
use rusqlite::params;

use super::{activity_columns, allow_target};
use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::lead_for_run;
use crate::workspace::advance_queue;
use crate::{SqliteStore, StoreError};

pub(super) const COMPANION_FRESH_MS: i64 = 30_000;

mod delivery;
mod workflow;

impl ClaudeStore for SqliteStore {
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
        "INSERT INTO claude_companions(session_key,generation,sequence,seen_at_ms,turn_id,latest_prompt_id,model,effort,latest_prompt_turn_id,latest_prompt_manual)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(session_key) DO UPDATE SET
            generation=excluded.generation, sequence=excluded.sequence, seen_at_ms=excluded.seen_at_ms,
            turn_id=excluded.turn_id, latest_prompt_id=excluded.latest_prompt_id,
            model=excluded.model,effort=excluded.effort,
            latest_prompt_turn_id=excluded.latest_prompt_turn_id,latest_prompt_manual=excluded.latest_prompt_manual
         WHERE (generation=excluded.generation AND sequence<excluded.sequence) OR
               (generation<>excluded.generation AND seen_at_ms<=?11)",
        params![key, observation.generation, observation.sequence as i64, now,
            observation.turn.as_ref().map(|value| value.as_str()),
            observation.latest_prompt.as_ref().map(|value| value.as_str()),
            observation.model, observation.effort,
            observation.latest_prompt_turn.as_ref().map(|value| value.as_str()),
            observation.latest_prompt_manual, now.saturating_sub(COMPANION_FRESH_MS)],
    )?;
    if recorded == 0 {
        return Err(AppError::StaleRevision.into());
    }
    if let Some(activity) = observation.activity {
        let (phase, questions) = activity_columns(Some(activity));
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
