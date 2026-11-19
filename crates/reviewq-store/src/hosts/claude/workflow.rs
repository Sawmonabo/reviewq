use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::hosts::{ClaudeTurnResult, ClaudeWorkflowContext, ClaudeWorkflowStore};
use reviewq_domain::identity::NativeTurnId;
use reviewq_domain::session::{HostProvider, LeadTarget};
use rusqlite::{Connection, OptionalExtension, params};

use super::{COMPANION_FRESH_MS, delivery::fresh};
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::history::history_session;
use crate::hosts::allow_target;
use crate::queue::{lead_for_run, parse};
use crate::{SqliteStore, StoreError};

impl ClaudeWorkflowStore for SqliteStore {
    fn claude_workflow_context(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
    ) -> AppFuture<'_, ClaudeWorkflowContext> {
        Box::pin(async move {
            self.read(move |db| context(db, lead, turn))
                .await
                .map_err(StoreError::app)
        })
    }

    fn record_claude_turn(&self, lead: LeadTarget, result: ClaudeTurnResult) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| finish_turn(tx, &lead, result))
                .await
                .map_err(StoreError::app)
        })
    }
}

fn context(
    db: &Connection,
    lead: LeadTarget,
    turn: Option<NativeTurnId>,
) -> Result<ClaudeWorkflowContext, StoreError> {
    ensure_lead(db, &lead)?;
    let session = history_session(
        db,
        &Actor::native_session(lead.binding().identity().clone()),
        lead.run_id().clone(),
        lead.session_key().clone(),
    )?;
    let key = lead.session_key().as_str();
    let live = db
        .query_row(
            "SELECT c.generation, c.turn_id,
           CASE WHEN s.host_activity='working' AND c.turn_id=c.latest_prompt_turn_id
                     AND (c.latest_prompt_manual=1 OR EXISTS (
                           SELECT 1 FROM messages m WHERE m.session_key=c.session_key AND m.run_id=s.run_id
                             AND m.native_turn_id=c.latest_prompt_turn_id AND m.native_input_id=c.latest_prompt_id
                             AND m.state='started' AND m.actor_key IS NOT NULL AND m.purpose IN ('user','report_feedback')
                     )) THEN c.latest_prompt_id END,
           coalesce(s.host_activity='working',0),
           coalesce(s.host_activity='idle' AND s.host_questions=0 AND c.permission_turn_id IS NULL,0)
         FROM claude_companions c JOIN sessions s USING(session_key)
         WHERE c.session_key=?1 AND c.seen_at_ms>?2",
            params![key, now_ms()?.saturating_sub(COMPANION_FRESH_MS)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            },
        )
        .optional()?;
    let (active_turn, request, ready, idle) =
        if let Some((generation, seen, request, working, idle)) = live {
            (
                seen.filter(|_| working).as_deref().map(parse).transpose()?,
                request.as_deref().map(parse).transpose()?,
                fresh(db, &lead, &generation)?.1,
                idle,
            )
        } else {
            (None, None, None, false)
        };
    let terminal = if let Some(turn) = turn {
        let row = db
            .query_row(
                "SELECT companion_generation,normal,response_id,response_sha256 FROM claude_turns
             WHERE session_key=?1 AND turn_id=?2",
                params![key, turn.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(generation, normal, id, hash)| {
            let response = match (id, hash) {
                (Some(id), Some(hash)) => Some((parse(&id)?, parse(&hash)?)),
                (None, None) => None,
                _ => return Err(StoreError::from(AppError::Internal)),
            };
            Ok(ClaudeTurnResult {
                turn,
                generation,
                normal,
                response,
            })
        })
        .transpose()?
    } else {
        None
    };
    Ok(ClaudeWorkflowContext {
        session,
        active_turn,
        idle,
        request,
        ready,
        terminal,
    })
}

fn finish_turn(
    tx: &mut WriteTx<'_>,
    lead: &LeadTarget,
    result: ClaudeTurnResult,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    ensure_lead(db, lead)?;
    let key = lead.session_key().as_str();
    let id = result.turn.as_str();
    let live: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM claude_companions WHERE session_key=?1 AND turn_id=?2 AND generation=?3)",
        params![key,id,result.generation], |row| row.get(0),
    )?;
    if !live {
        return Err(AppError::Conflict.into());
    }
    let response = result.response.as_ref();
    let written = db.execute(
        "INSERT INTO claude_turns(session_key,turn_id,companion_generation,normal,response_id,response_sha256,recorded_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(session_key,turn_id) DO NOTHING",
        params![key,id,result.generation,result.normal,response.map(|r|r.0.as_str()),response.map(|r|r.1.as_str()),now_ms()?],
    )?;
    if written == 0 {
        let same: bool = db.query_row(
            "SELECT companion_generation=?3 AND normal=?4 AND response_id IS ?5 AND response_sha256 IS ?6
             FROM claude_turns WHERE session_key=?1 AND turn_id=?2",
            params![key,id,result.generation,result.normal,response.map(|r|r.0.as_str()),response.map(|r|r.1.as_str())],
            |row| row.get(0),
        )?;
        if !same {
            return Err(AppError::Conflict.into());
        }
    }
    db.execute(
        "UPDATE claude_companions SET permission_turn_id=NULL
         WHERE session_key=?1 AND turn_id=?2 AND generation=?3 AND permission_turn_id=?2",
        params![key, id, result.generation],
    )?;
    tx.report_changed = written != 0;
    Ok(())
}

pub(super) fn ensure_lead(db: &Connection, lead: &LeadTarget) -> Result<(), StoreError> {
    let native = lead.binding().identity();
    if native.provider() != HostProvider::Claude
        || lead_for_run(db, lead.run_id())?.as_ref() != Some(lead)
    {
        return Err(AppError::NotFound.into());
    }
    allow_target(db, &Actor::native_session(native.clone()), lead)
}
