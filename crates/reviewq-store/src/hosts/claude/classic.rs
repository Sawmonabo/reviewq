use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::hosts::ClaudeClassicEvent;
use reviewq_domain::identity::{NotificationId, RunId, SessionKey};
use reviewq_domain::session::LeadTarget;
use rusqlite::{Connection, OptionalExtension, params};

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
    // Claude runs this hook as a command from the running session, and a hook that fails is
    // not sent again, so the event shows the session present and is accepted until the
    // companion counts as gone. While the companion is live, its stored turn is the one
    // running; while it is unknown, that turn may be out of date.
    let companion: Option<(bool, Option<String>)> = db
        .query_row(
            "SELECT reviewq_companion_live(c.seen_at_ms),
               CASE WHEN s.host_activity IN ('working','blocked') THEN c.turn_id END
             FROM claude_companions c JOIN sessions s USING(session_key)
             WHERE c.session_key=?1 AND NOT reviewq_companion_gone(c.seen_at_ms)",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((live, active)) = companion else {
        return Err(AppError::Unavailable.into());
    };
    if !live && matches!(event, ClaudeClassicEvent::PermissionPending) {
        // Stored unbound and shown as blocked now; the companion's next report binds it to the
        // turn and step it names and raises the alert for that turn.
        let changed = db.execute(
            "UPDATE claude_companions SET permission_unbound=1 WHERE session_key=?1 AND permission_unbound=0",
            [key],
        )?;
        if changed != 0 {
            crate::hosts::record_activity(db, key, Some("blocked"), true)?;
            affected(tx, &lead)?;
        }
        return Ok(());
    }
    let Some(active) = active else {
        return Err(AppError::Unavailable.into());
    };
    match event {
        ClaudeClassicEvent::PermissionPending => {
            let changed = db.execute(
                "UPDATE claude_companions SET permission_turn_id=?2,permission_step=turn_step,
                   permission_unbound=0
                 WHERE session_key=?1 AND permission_turn_id IS NOT ?2
                   AND NOT EXISTS(SELECT 1 FROM claude_turns WHERE session_key=?1 AND turn_id=?2)",
                params![key, active],
            )?;
            if changed != 0 {
                crate::hosts::record_activity(db, key, Some("blocked"), true)?;
                if let Some(event) = permission_alert(db, &lead, active)? {
                    crate::notifications::publish(tx, &[event])?;
                }
                affected(tx, &lead)?;
            }
        }
        ClaudeClassicEvent::Display {
            message,
            index,
            final_chunk,
        } => {
            if index != 0 && !final_chunk {
                return Ok(());
            }
            let changed = db.execute(
                "INSERT INTO claude_display_events(session_key,turn_id,display_id,first_seen,final_seen)
                 VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(session_key,turn_id,display_id) DO UPDATE SET
                    first_seen=max(first_seen,excluded.first_seen), final_seen=max(final_seen,excluded.final_seen)
                 WHERE first_seen<excluded.first_seen OR final_seen<excluded.final_seen",
                params![key,active,message.as_str(),index==0,final_chunk],
            )?;
            if changed != 0 {
                tx.emit(&[ResourceKey::History(Some(lead.session_key().clone()))])?;
            }
        }
        _ => return Err(AppError::Conflict.into()),
    }
    Ok(())
}

/// Raises the alert that `lead`'s session waits on permission in `turn`, once per turn.
pub(super) fn permission_alert(
    db: &Connection,
    lead: &LeadTarget,
    turn: String,
) -> Result<Option<NotificationId>, StoreError> {
    let occurrence = reviewq_app::notifications::NativeOccurrence {
        kind: reviewq_app::notifications::Kind::AgentPermission,
        native_ref: turn,
        at_ms: Some(now_ms()?),
        desktop_episode: None,
    };
    crate::notifications::native::insert(
        db,
        reviewq_domain::session::HostProvider::Claude,
        lead.target(),
        occurrence,
    )
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
