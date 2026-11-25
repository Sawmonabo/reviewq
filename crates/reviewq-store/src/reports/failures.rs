//! Report failures keep their native address independently of the current report.

use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_app::workflow::NativeCall;
use reviewq_domain::identity::NotificationId;
use rusqlite::{Connection, params};

use crate::StoreError;
use crate::notifications::insert_notification;

/// Records one failure of `kind` (`unfinished_review`, `closing_refused` or
/// `update_failed`) for the call's run and optional pass, with its notification. A repeated
/// occurrence inserts nothing and returns `None`.
#[expect(dead_code, reason = "K2 records failures by pass and kind")]
pub(crate) fn note(
    db: &Connection,
    call: &NativeCall,
    pass: Option<&str>,
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
            label: "Report could not be published".to_owned(),
            occurred_at_ms: time,
            desktop_episode: None,
        },
    )?;
    if let Some(notification) = &notification {
        db.execute(
            "INSERT INTO report_delivery_failures(notification_id,run_id,lead_session_key,
                native_turn_id,native_call_id,pass_id,kind,reason,occurred_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                notification.as_str(),
                call.lead.run_id().as_str(),
                call.lead.session_key().as_str(),
                call.turn_id.as_str(),
                call.call_id,
                pass,
                kind,
                reason,
                time
            ],
        )?;
    }
    Ok(notification)
}

/// Resolves every unresolved failure on `run` with the receipt a publish just recorded. The
/// failure rows are kept.
#[expect(
    dead_code,
    reason = "K2 resolves earlier failures inside the publish commit"
)]
pub(crate) fn resolve(db: &Connection, run: &str, receipt: &str) -> Result<(), StoreError> {
    db.prepare_cached(
        "UPDATE report_delivery_failures SET resolved_by_receipt_id=?2
         WHERE run_id=?1 AND resolved_by_receipt_id IS NULL",
    )?
    .execute(params![run, receipt])?;
    Ok(())
}
