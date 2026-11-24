//! Failed handoffs retain their native address independently of the current report.

use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_app::workflow::NativeCall;
use reviewq_domain::identity::NotificationId;
use rusqlite::{Connection, params};

use crate::StoreError;
use crate::notifications::insert_notification;

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

/// Resolves every unresolved failure on the run that came before the accepted handoff
/// `receipt`, which left the run with a current report. The failures stay listed.
///
/// A failure takes its native position from the handoff or correction request at its
/// address. Without a comparable position, that attempt (or the failure itself) must
/// predate the accepted handoff's staging, so a later attempt's failure stays unresolved.
pub(super) fn resolve(db: &Connection, receipt: &str) -> Result<(), StoreError> {
    db.prepare_cached(
        "UPDATE report_delivery_failures SET resolved_by_receipt_id=?1 WHERE notification_id IN (
           SELECT f.notification_id FROM report_handoffs a
           JOIN report_delivery_failures f ON f.run_id=a.run_id AND f.resolved_by_receipt_id IS NULL
           LEFT JOIN report_handoffs h ON (h.run_id,h.lead_session_key,h.turn_id,h.call_id)=
               (f.run_id,f.lead_session_key,f.native_turn_id,f.native_call_id)
           LEFT JOIN report_correction_requests c ON (c.run_id,c.lead_session_key,c.turn_id,c.call_id)=
               (f.run_id,f.lead_session_key,f.native_turn_id,f.native_call_id)
           WHERE a.receipt_id=?1 AND coalesce(
             CASE WHEN coalesce(h.source_generation,c.source_generation) IS NOT NULL THEN
               reviewq_report_order(coalesce(h.source_generation,c.source_generation),
                 coalesce(h.source_position,c.source_position),a.source_generation,a.source_position)<=0 END,
             coalesce(h.created_at_ms,c.requested_at_ms,f.occurred_at_ms)<=a.created_at_ms))",
    )?
    .execute([receipt])?;
    Ok(())
}
