//! Delivery failures: one unresolved row per run, pass and kind, with its notification, until
//! a publish resolves it.

use reviewq_app::ledger::FailureRecord;
use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_domain::identity::NotificationId;
use reviewq_domain::session::LeadTarget;
use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::StoreError;
use crate::notifications::insert_notification;

/// What recording a failure did.
pub(crate) enum Noted {
    /// The unresolved failure of its run, pass and kind was updated in place.
    Repeated,
    /// A new failure, with the notification it raised; `None` when it is stored resolved
    /// because its pass is already closed.
    First(Option<NotificationId>),
}

/// Records `failure` for `lead`'s run once per run, pass and kind. A repeat updates the
/// reason, sequence and turn in place; a failure whose pass is closed is stored resolved and
/// raises nothing.
pub(crate) fn note(
    db: &Connection,
    lead: &LeadTarget,
    failure: &FailureRecord,
    time: i64,
) -> Result<Noted, StoreError> {
    let run = failure.run.as_str();
    let pass = failure.pass.map(|pass| pass.get());
    let kind = failure.kind.as_str();
    let turn = failure.turn.as_ref().map(|turn| turn.as_str());
    let sequence = failure.sequence.as_sqlite_integer();
    let repeated = db
        .prepare_cached(
            "UPDATE report_delivery_failures SET reason=?4, ledger_sequence=?5,
               native_turn_id=coalesce(?6, native_turn_id), updated_at_ms=max(updated_at_ms, ?7)
             WHERE run_id=?1 AND coalesce(pass_id, 0)=coalesce(?2, 0) AND kind=?3
               AND resolved_at_ms IS NULL",
        )?
        .execute(params![
            run,
            pass,
            kind,
            failure.reason,
            sequence,
            turn,
            time
        ])?;
    if repeated != 0 {
        return Ok(Noted::Repeated);
    }
    let closed: bool = db
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM review_passes WHERE pass_id=?1 AND state='closed')",
        )?
        .query_row([pass], |row| row.get(0))?;
    let notification = if closed {
        None
    } else {
        let occurrence = Uuid::new_v4().to_string();
        insert_notification(
            db,
            Observation {
                source: Source::Report,
                occurrence_id: occurrence.clone(),
                kind: Kind::ReportFailed,
                review: lead.review_id().clone(),
                run: Some(lead.run_id().clone()),
                session: Some(lead.session_key().clone()),
                event_ref: Some(occurrence),
                label: "Report could not be published".to_owned(),
                occurred_at_ms: time,
                desktop_episode: None,
            },
        )?
    };
    db.prepare_cached(
        "INSERT INTO report_delivery_failures(notification_id, run_id, pass_id, kind, native_turn_id,
           reason, ledger_sequence, occurred_at_ms, updated_at_ms, resolved_at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, CASE WHEN ?9 THEN ?8 END)",
    )?
    .execute(params![
        notification.as_ref().map(|id| id.as_str()),
        run,
        pass,
        kind,
        turn,
        failure.reason,
        sequence,
        time,
        closed
    ])?;
    Ok(Noted::First(notification))
}

/// Resolves every unresolved failure on `run` with the receipt a publish just recorded or
/// kept. The failure rows are kept.
pub(crate) fn resolve(
    db: &Connection,
    run: &str,
    receipt: &str,
    time: i64,
) -> Result<bool, StoreError> {
    let changed = db
        .prepare_cached(
            "UPDATE report_delivery_failures SET resolved_at_ms=?3, resolved_by_receipt_id=?2
             WHERE run_id=?1 AND resolved_at_ms IS NULL",
        )?
        .execute(params![run, receipt, time])?;
    Ok(changed != 0)
}
