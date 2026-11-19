use reviewq_app::access::AppError;
use reviewq_app::followups::ExecutingCheck;
use rusqlite::{Connection, params};

use super::emit;
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::queue::{lead_for_run, parse};

const AWAITING_IDLE: &str = "
    b.state='executing' AND b.authority_verified=1 AND b.native_turn_id IS NOT NULL
    AND (r.state='open' AND b.holds_slot=1 AND b.hold_reason='followup_native_input_required'
         OR r.state IN ('closed','merged') AND NOT EXISTS (
             SELECT 1 FROM report_handoffs h WHERE h.batch_id=b.batch_id AND h.state='pending_completion'
         ))";

pub(super) fn checks(db: &Connection) -> Result<Vec<ExecutingCheck>, StoreError> {
    let sql = format!(
        "SELECT b.batch_id,b.run_id,b.lead_session_key,coalesce(b.continuation_turn_id,b.native_turn_id)
           FROM followup_batches b JOIN reviews r USING(review_id)
           WHERE {AWAITING_IDLE} ORDER BY b.holds_slot DESC,b.latest_observed_at_ms,b.batch_id LIMIT 8"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut output = Vec::new();
    for row in rows {
        let (batch_id, run, session, turn) = row?;
        let Some(lead) = lead_for_run(db, &parse(&run)?)? else {
            continue;
        };
        if lead.session_key().as_str() != session {
            continue;
        }
        output.push(ExecutingCheck {
            batch_id,
            lead,
            turn: parse(&turn)?,
        });
    }
    Ok(output)
}

pub(super) fn observe(tx: &mut WriteTx<'_>, check: ExecutingCheck) -> Result<(), StoreError> {
    let lead = &check.lead;
    if lead_for_run(&tx.sql, lead.run_id())?.as_ref() != Some(lead) {
        return Err(AppError::StaleRevision.into());
    }
    let sql = format!(
        "UPDATE followup_batches AS x SET holds_slot=0,
            state=CASE WHEN r.state IN ('closed','merged') THEN 'cancelled' ELSE x.state END,
            hold_reason=CASE WHEN r.state IN ('closed','merged') THEN 'merge_request_closed' ELSE x.hold_reason END,
            completed_at_ms=CASE WHEN r.state IN ('closed','merged') THEN ?5 ELSE x.completed_at_ms END
         FROM followup_batches b JOIN reviews r USING(review_id)
         WHERE x.batch_id=b.batch_id AND b.batch_id=?1 AND b.run_id=?2 AND b.lead_session_key=?3
            AND coalesce(b.continuation_turn_id,b.native_turn_id)=?4 AND {AWAITING_IDLE}"
    );
    let updated = tx.sql.execute(
        &sql,
        params![
            check.batch_id,
            lead.run_id().as_str(),
            lead.session_key().as_str(),
            check.turn.as_str(),
            now_ms()?
        ],
    )?;
    if updated != 0 {
        emit(tx, lead.review_id(), lead.run_id())?;
    }
    Ok(())
}
