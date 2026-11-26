use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::hosts::ClaudeClassicEvent;
use reviewq_domain::identity::{RunId, SessionKey};
use reviewq_domain::session::LeadTarget;
use rusqlite::{OptionalExtension, params};

use super::workflow::ensure_lead;
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::workspace::advance_queue;

pub(super) fn record(
    tx: &mut WriteTx<'_>,
    lead: LeadTarget,
    event: ClaudeClassicEvent,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    ensure_lead(db, &lead)?;
    let key = lead.session_key().as_str();
    if let ClaudeClassicEvent::Disconnected { generation } = event {
        store_ended(tx, lead.run_id(), lead.session_key(), &generation, false)?;
        return Ok(());
    }
    // Claude runs this hook inside the session's own process, so the event itself shows the
    // session present, and a hook that fails is not sent again. It is refused only once the
    // companion counts as gone, when its stored turn no longer says which turn is running.
    let active: Option<String> = db
        .query_row(
            "SELECT c.turn_id FROM claude_companions c JOIN sessions s USING(session_key)
         WHERE c.session_key=?1 AND NOT reviewq_companion_gone(c.seen_at_ms) AND c.turn_id IS NOT NULL
           AND s.host_activity IN ('working','blocked')",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    let Some(active) = active else {
        return Err(AppError::Unavailable.into());
    };
    match event {
        ClaudeClassicEvent::PermissionPending => {
            let changed = db.execute(
                "UPDATE claude_companions SET permission_turn_id=?2,permission_step=turn_step
                 WHERE session_key=?1 AND permission_turn_id IS NOT ?2
                   AND NOT EXISTS(SELECT 1 FROM claude_turns WHERE session_key=?1 AND turn_id=?2)",
                params![key, active],
            )?;
            if changed != 0 {
                crate::hosts::record_activity(db, key, Some("blocked"), true)?;
                let occurrence = reviewq_app::notifications::NativeOccurrence {
                    kind: reviewq_app::notifications::Kind::AgentPermission,
                    native_ref: active,
                    at_ms: Some(now_ms()?),
                    desktop_episode: None,
                };
                let event = crate::notifications::native::insert(
                    db,
                    reviewq_domain::session::HostProvider::Claude,
                    lead.target(),
                    occurrence,
                )?;
                if let Some(event) = event {
                    crate::notifications::publish(tx, &[event])?;
                }
                affected(tx, &lead)?;
            }
        }
        ClaudeClassicEvent::Display {
            turn,
            message,
            index,
            final_chunk,
        } if turn.as_str() == active => {
            if index != 0 && !final_chunk {
                return Ok(());
            }
            let changed = db.execute(
                "INSERT INTO claude_display_events(session_key,turn_id,display_id,first_seen,final_seen)
                 VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(session_key,turn_id,display_id) DO UPDATE SET
                    first_seen=max(first_seen,excluded.first_seen), final_seen=max(final_seen,excluded.final_seen)
                 WHERE first_seen<excluded.first_seen OR final_seen<excluded.final_seen",
                params![key,turn.as_str(),message.as_str(),index==0,final_chunk],
            )?;
            if changed != 0 {
                tx.emit(&[ResourceKey::History(Some(lead.session_key().clone()))])?;
            }
        }
        _ => return Err(AppError::Conflict.into()),
    }
    Ok(())
}

/// Stores `generation` of the companion of `session` as ended, which counts as gone at once,
/// and announces the change. A companion that said it disconnected is ended as it is; with
/// `if_silent`, as the tick ends companions that went silent, only while it still counts as
/// gone, because a report may have landed since the tick read it. The next report of the same
/// generation makes it live again, and a different generation may take it over at once.
pub(super) fn store_ended(
    tx: &mut WriteTx<'_>,
    run: &RunId,
    session: &SessionKey,
    generation: &str,
    if_silent: bool,
) -> Result<(), StoreError> {
    let changed = tx.sql.execute(
        "UPDATE claude_companions SET seen_at_ms=0 WHERE session_key=?1 AND generation=?2
           AND seen_at_ms<>0 AND (?3=0 OR reviewq_companion_gone(seen_at_ms))",
        params![session.as_str(), generation, if_silent],
    )?;
    if changed != 0 {
        announce(tx, run, session)?;
    }
    Ok(())
}

fn affected(tx: &mut WriteTx<'_>, lead: &LeadTarget) -> Result<(), StoreError> {
    announce(tx, lead.run_id(), lead.session_key())
}

fn announce(tx: &mut WriteTx<'_>, run: &RunId, session: &SessionKey) -> Result<(), StoreError> {
    advance_queue(&tx.sql)?;
    tx.emit(&[
        ResourceKey::Queue,
        ResourceKey::Run(Some(run.clone())),
        ResourceKey::Session(Some(session.clone())),
    ])?;
    Ok(())
}
