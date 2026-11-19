use reviewq_app::access::{Actor, AppError};
use reviewq_app::history::FilePosition;
use reviewq_app::hosts::{
    ClaudeAnchor, ClaudeDeliveryResult, ClaudeOffer, ClaudeSend, DeliveryClaim,
    DeliveryObservation, DeliveryWork, RecoverableDeliveryState,
};
use reviewq_app::workflow::ReadyWorkflow;
use reviewq_domain::session::LeadTarget;
use rusqlite::{Connection, OptionalExtension, params};

use super::COMPANION_FRESH_MS;
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::hosts::{allow_target, delivery};
use crate::queue::parse;

pub(super) fn offer(
    tx: &mut WriteTx<'_>,
    lead: LeadTarget,
    generation: String,
) -> Result<Option<ClaudeOffer>, StoreError> {
    let now = now_ms()?;
    let key = lead.session_key().as_str();
    if let Some((id, fence, prior, authorized, expired, state)) = tx
        .sql
        .query_row(
            "SELECT c.delivery_id,c.fence,c.generation,c.authorized_at_ms IS NOT NULL,
                o.due_at_ms<=?2,m.state
         FROM claude_deliveries c JOIN outbox o USING(delivery_id)
         JOIN messages m ON m.message_id=c.delivery_id
         WHERE c.session_key=?1 AND c.finished=0",
            params![key, now],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?
    {
        let offer = ClaudeOffer {
            id: parse(&id)?,
            fence: fence.try_into().map_err(|_| AppError::Internal)?,
            generation: prior,
        };
        if expired && state == "dispatching" {
            let result = if authorized {
                ClaudeDeliveryResult::Uncertain
            } else {
                ClaudeDeliveryResult::Deferred
            };
            finish(tx, lead, offer, result)?;
            return Ok(None);
        }
        if !authorized && offer.generation == generation && fresh(&tx.sql, &lead, &generation)?.0 {
            return Ok(Some(offer));
        }
        return Ok(None);
    }
    let (idle, ready) = fresh(&tx.sql, &lead, &generation)?;
    if !idle {
        return Ok(None);
    }
    let query = format!(
        "SELECT m.message_id,m.purpose {} AND m.run_id=?2 AND m.session_key=?3
         AND m.state='queued' AND o.due_at_ms<=?4 ORDER BY o.due_at_ms,o.delivery_id LIMIT 1",
        delivery::SCHEDULE_FROM,
    );
    let next = tx
        .sql
        .query_row(
            &query,
            params!["claude", lead.run_id().as_str(), key, now],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((id, purpose)) = next else {
        return Ok(None);
    };
    if ready.is_none() && matches!(purpose.as_str(), "publication" | "thread_followup") {
        return Ok(None);
    }
    let Some(claim) = delivery::claim(tx, parse(&id)?, RecoverableDeliveryState::Queued)? else {
        return Ok(None);
    };
    tx.sql.execute(
        "INSERT INTO claude_deliveries(delivery_id,session_key,fence,generation)
         VALUES(?1,?2,?3,?4)
         ON CONFLICT(delivery_id) DO UPDATE SET fence=excluded.fence,generation=excluded.generation,
           authorized_at_ms=NULL,anchor_generation=NULL,anchor_offset=NULL,anchor_digest=NULL,finished=0
         WHERE claude_deliveries.finished=1 AND claude_deliveries.session_key=excluded.session_key",
        params![id,key,claim.fence.as_sqlite_integer(),generation],
    )?;
    Ok(Some(ClaudeOffer {
        id: claim.delivery_id,
        fence: claim.fence,
        generation,
    }))
}

pub(super) fn read(
    db: &Connection,
    lead: LeadTarget,
    offer: &ClaudeOffer,
) -> Result<ClaudeSend, StoreError> {
    allow_target(
        db,
        &Actor::native_session(lead.binding().identity().clone()),
        &lead,
    )?;
    let anchor = db
        .query_row(
            "SELECT anchor_generation,anchor_offset,anchor_digest FROM claude_deliveries
         WHERE delivery_id=?1 AND session_key=?2 AND fence=?3 AND generation=?4 AND finished=0",
            params![
                offer.id.as_str(),
                lead.session_key().as_str(),
                offer.fence.as_sqlite_integer(),
                offer.generation
            ],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppError::Conflict)?;
    let anchor = match anchor {
        (Some(source_generation), Some(offset), Some(witness)) => Some(ClaudeAnchor {
            source_generation,
            file: FilePosition {
                offset: offset.try_into().map_err(|_| AppError::Internal)?,
                high_water: offset.try_into().map_err(|_| AppError::Internal)?,
                witness: witness.try_into().map_err(|_| AppError::Internal)?,
            },
        }),
        (None, None, None) => None,
        _ => return Err(AppError::Internal.into()),
    };
    let ready = fresh(db, &lead, &offer.generation)?.1;
    let claim = delivery::existing_dispatch(db, offer.id.clone(), offer.fence, lead)?;
    Ok(ClaudeSend {
        claim,
        anchor,
        ready,
    })
}

pub(super) fn authorize(
    tx: &mut WriteTx<'_>,
    lead: LeadTarget,
    offer: ClaudeOffer,
    anchor: ClaudeAnchor,
    checked: Option<ReadyWorkflow>,
) -> Result<DeliveryClaim, StoreError> {
    let db = &tx.sql;
    let send = read(db, lead.clone(), &offer)?;
    if send.anchor.is_some() || !fresh(db, &lead, &offer.generation)?.0 {
        return Err(AppError::Conflict.into());
    }
    if let DeliveryWork::Dispatch {
        workflow: Some(guard),
        ..
    } = &send.claim.work
        && (checked.as_ref() != Some(&guard.ready) || checked != send.ready)
    {
        return Err(AppError::Conflict.into());
    }
    delivery::authorize_dispatch(db, send.claim.clone())?;
    if anchor.file.offset != anchor.file.high_water {
        return Err(AppError::InvalidRequest.into());
    }
    let changed = db.execute(
        "UPDATE claude_deliveries SET
            authorized_at_ms=?2,anchor_generation=?3,anchor_offset=?4,anchor_digest=?5
         WHERE delivery_id=?1 AND authorized_at_ms IS NULL AND finished=0",
        params![
            offer.id.as_str(),
            now_ms()?,
            anchor.source_generation,
            i64::try_from(anchor.file.offset).map_err(|_| AppError::InvalidRequest)?,
            anchor.file.witness.as_slice()
        ],
    )?;
    if changed != 1 {
        return Err(AppError::Conflict.into());
    }
    Ok(send.claim)
}

pub(super) fn finish(
    tx: &mut WriteTx<'_>,
    lead: LeadTarget,
    offer: ClaudeOffer,
    result: ClaudeDeliveryResult,
) -> Result<(), StoreError> {
    let send = read(&tx.sql, lead, &offer)?;
    let (observation, terminal) = match result {
        ClaudeDeliveryResult::Deferred if send.anchor.is_none() => {
            (DeliveryObservation::NotSubmitted, true)
        }
        ClaudeDeliveryResult::Dropped if send.anchor.is_some() => {
            (DeliveryObservation::NotSubmitted, true)
        }
        ClaudeDeliveryResult::Started { anchor, input }
            if send.anchor.as_ref() == Some(&anchor) =>
        {
            (
                DeliveryObservation::Started {
                    delivery_id: offer.id.clone(),
                    input,
                },
                true,
            )
        }
        ClaudeDeliveryResult::Uncertain if send.anchor.is_some() => {
            (DeliveryObservation::Uncertain, false)
        }
        _ => return Err(AppError::Conflict.into()),
    };
    let state: String = tx.sql.query_row(
        "SELECT state FROM messages WHERE message_id=?1",
        [offer.id.as_str()],
        |row| row.get(0),
    )?;
    let mut claim = send.claim;
    if state == "uncertain" {
        if !terminal {
            return Ok(());
        }
        if !matches!(observation, DeliveryObservation::Started { .. }) {
            return Err(AppError::Conflict.into());
        }
        let changed = tx.sql.execute(
            "UPDATE outbox SET claim_kind='reconcile' WHERE delivery_id=?1 AND fence=?2 AND claim_kind IS NULL",
            params![offer.id.as_str(),offer.fence.as_sqlite_integer()],
        )?;
        if changed != 1 {
            return Err(AppError::Conflict.into());
        }
        claim.work = DeliveryWork::Reconcile { cursor: None };
    }
    delivery::finish(tx, claim, observation)?;
    if terminal {
        tx.sql.execute(
            "UPDATE claude_deliveries SET finished=1 WHERE delivery_id=?1",
            [offer.id.as_str()],
        )?;
    }
    Ok(())
}

fn fresh(
    db: &Connection,
    lead: &LeadTarget,
    generation: &str,
) -> Result<(bool, Option<ReadyWorkflow>), StoreError> {
    let value = db.query_row(
        "SELECT s.host_activity='idle' AND s.host_questions=0,c.model,c.effort,c.latest_prompt_id,
                c.latest_prompt_turn_id
         FROM claude_companions c JOIN sessions s USING(session_key)
         WHERE c.session_key=?1 AND c.generation=?2 AND c.seen_at_ms>?3 AND s.run_id=?4",
        params![lead.session_key().as_str(),generation,now_ms()?.saturating_sub(COMPANION_FRESH_MS),lead.run_id().as_str()],
        |row| Ok((row.get::<_, Option<bool>>(0)?,row.get::<_, Option<String>>(1)?,row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,row.get::<_, Option<String>>(4)?)),
    ).optional()?;
    let Some((Some(true), model, effort, prompt, turn)) = value else {
        return Ok((false, None));
    };
    if let (Some(model), Some(effort), Some(prompt), Some(turn)) = (model, effort, prompt, turn) {
        return Ok((
            true,
            Some(ReadyWorkflow {
                model,
                effort,
                latest_user_entry_id: Some(parse(&prompt)?),
                latest_user_turn_id: Some(parse(&turn)?),
            }),
        ));
    }
    Ok((true, None))
}
