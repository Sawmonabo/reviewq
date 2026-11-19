//! Failed handoffs retain their native address independently of the current report.

use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_app::workflow::{NativeCall, ReportFailureAddress};
use reviewq_domain::identity::{GitSha, NotificationId};
use rusqlite::{Connection, params};

use super::{ReportWork, db_int};
use crate::StoreError;
use crate::notifications::insert_notification;
use crate::queue::{parse, text};

pub(super) fn note(
    db: &Connection,
    call: &NativeCall,
    occurrence: String,
    reason: &str,
    kind: &'static str,
    time: i64,
) -> Result<Option<NotificationId>, StoreError> {
    let notification = insert_notification(
        db,
        Observation {
            source: Source::Report,
            occurrence_id: occurrence.clone(),
            kind: Kind::ReportFailed,
            review: call.lead.review_id().clone(),
            run: Some(call.lead.run_id().clone()),
            session: Some(call.lead.session_key().clone()),
            event_ref: Some(occurrence),
            label: "Report could not be accepted".to_owned(),
            occurred_at_ms: time,
            desktop_episode: None,
        },
    )?;
    if let Some(notification) = &notification {
        db.execute(
            "INSERT INTO report_delivery_failures(notification_id,run_id,lead_session_key,
                native_turn_id,native_call_id,kind,identity_verified,reason,occurred_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,1,?7,?8)",
            params![
                notification.as_str(),
                call.lead.run_id().as_str(),
                call.lead.session_key().as_str(),
                call.turn_id.as_str(),
                call.call_id,
                kind,
                reason,
                time
            ],
        )?;
    }
    Ok(notification)
}

pub(super) fn unresolved(
    db: &Connection,
    call: &NativeCall,
) -> Result<Vec<ReportFailureAddress>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT notification_id,native_turn_id,native_call_id FROM report_delivery_failures
         WHERE run_id=?1 AND lead_session_key=?2 AND resolved_by_receipt_id IS NULL
           AND native_call_id IS NOT NULL AND occurred_at_ms<=(SELECT created_at_ms FROM report_handoffs
              WHERE (run_id,lead_session_key,turn_id,call_id)=(?1,?2,?3,?4))
         ORDER BY occurred_at_ms DESC,notification_id DESC LIMIT 32",
    )?;
    let mut rows = statement.query(params![
        call.lead.run_id().as_str(),
        call.lead.session_key().as_str(),
        call.turn_id.as_str(),
        call.call_id,
    ])?;
    let mut found = Vec::new();
    while let Some(row) = rows.next()? {
        // Older unsupported bridge addresses have no claim on a native receipt.
        if let Ok(item) = text(row, 2)?.parse() {
            found.push(ReportFailureAddress {
                notification: parse(text(row, 0)?)?,
                turn: parse(text(row, 1)?)?,
                item,
            });
        }
    }
    Ok(found)
}

pub(super) fn resolve(
    db: &Connection,
    work: &ReportWork,
    revision: &GitSha,
    proven: Vec<ReportFailureAddress>,
) -> Result<(), StoreError> {
    let call = &work.call;
    let receipt = work.acknowledgement.receipt_id.as_str();
    // Recheck that the installed report covers the current review and that this exact
    // immutable native handoff is the one under consideration.
    let current: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM reviews WHERE review_id=?1 AND head_sha=?2)",
        params![call.lead.review_id().as_str(), revision.as_str()],
        |row| row.get(0),
    )?;
    if !current {
        return Ok(());
    }
    let run = call.lead.run_id().as_str();
    let lead = call.lead.session_key().as_str();
    let position = db_int(call.source_position)?;
    db.execute(
        "UPDATE report_delivery_failures AS f SET resolved_by_receipt_id=?3
         WHERE (f.run_id,f.lead_session_key)=(?1,?2) AND f.resolved_by_receipt_id IS NULL
           AND f.identity_verified=1
           AND (
             EXISTS(SELECT 1 FROM report_handoffs h WHERE
                (h.run_id,h.lead_session_key,h.turn_id,h.call_id)=(f.run_id,f.lead_session_key,f.native_turn_id,f.native_call_id)
                AND h.state='failed' AND h.form='final'
                AND h.batch_id IS (SELECT batch_id FROM report_handoffs WHERE receipt_id=?3)
                AND h.source_generation=?4 AND h.source_position<?5)
             OR EXISTS(SELECT 1 FROM report_correction_requests c WHERE
                (c.run_id,c.lead_session_key,c.turn_id,c.call_id)=(f.run_id,f.lead_session_key,f.native_turn_id,f.native_call_id)
                AND c.source_generation=?4 AND ((c.source_position<?5 AND
                   (c.turn_id=?6 OR (SELECT batch_id FROM report_handoffs WHERE receipt_id=?3) IS NULL)) OR
                   (c.source_position=?5 AND c.turn_id=?6 AND c.call_id=?7)))
           )",
        params![run,lead,receipt,call.source_generation,position,call.turn_id.as_str(),call.call_id],
    )?;
    let mut update = db.prepare_cached(
        "UPDATE report_delivery_failures SET resolved_by_receipt_id=?1
         WHERE (notification_id,run_id,lead_session_key,native_turn_id,native_call_id)=(?2,?3,?4,?5,?6)
           AND resolved_by_receipt_id IS NULL AND identity_verified=0",
    )?;
    for address in proven {
        update.execute(params![
            receipt,
            address.notification.as_str(),
            run,
            lead,
            address.turn.as_str(),
            address.item.as_str()
        ])?;
    }
    Ok(())
}
