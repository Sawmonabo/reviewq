//! Per-review and focused native evidence for a bounded selected page.

use reviewq_app::access::AppError;
use reviewq_app::hosts::HostPhase;
use reviewq_app::queue::{FocusedLaunch, QueueItem};
use rusqlite::Connection;

use super::{parse, provider, text};
use crate::StoreError;

pub(super) fn hydrate<'a>(
    db: &Connection,
    items: impl Iterator<Item = &'a mut QueueItem>,
) -> Result<(), StoreError> {
    let mut items: Vec<_> = items.collect();
    if items.is_empty() {
        return Ok(());
    }
    let keys = items
        .iter()
        .enumerate()
        .map(|(slot, item)| {
            (
                slot,
                item.review_id.as_str(),
                item.signals.run.as_ref().map(|id| id.as_str()),
                item.signals.focus.as_ref().map(|id| id.as_str()),
            )
        })
        .collect::<Vec<_>>();
    let selected = serde_json::to_string(&keys).map_err(|_| AppError::Internal)?;
    let mut statement = db.prepare_cached(
        "SELECT p.value->>0,s.provider,
                (l.run_id IS NOT NULL OR s.native_model IS NOT NULL OR s.native_effort IS NOT NULL
                    OR c.model IS NOT NULL OR c.effort IS NOT NULL),
                CASE WHEN s.native_model IS NOT NULL OR s.native_effort IS NOT NULL
                       THEN s.native_model
                     WHEN c.model IS NOT NULL OR c.effort IS NOT NULL THEN c.model
                     ELSE l.observed_model END,
                CASE WHEN s.native_model IS NOT NULL OR s.native_effort IS NOT NULL
                       THEN s.native_effort
                     WHEN c.model IS NOT NULL OR c.effort IS NOT NULL THEN c.effort
                     ELSE l.observed_effort END,
                l.requested_model,l.requested_effort,v.reviewed_sha,
                (SELECT a.host_activity FROM review_active_sessions a WHERE a.session_key=s.session_key),
                (SELECT json_group_array(provider) FROM (
                    SELECT DISTINCT ss.provider FROM runs rr
                    JOIN sessions ss ON ss.run_id=rr.run_id AND ss.session_key=rr.lead_session_key
                    WHERE rr.review_id=(p.value->>1) LIMIT 2
                )),
                (SELECT count(*) FROM notifications n WHERE n.review_id=(p.value->>1)
                    AND n.read_at_ms IS NULL AND n.dismissed_at_ms IS NULL
                    AND NOT (n.source='gitlab' AND n.kind IN ('reply','edited_reply'))), lead.native_name
         FROM json_each(?1) p
         LEFT JOIN runs r ON r.run_id=(p.value->>2) AND r.review_id=(p.value->>1)
         LEFT JOIN sessions s ON s.session_key=(p.value->>3) AND s.run_id=r.run_id
         LEFT JOIN claude_companions c ON c.session_key=s.session_key
         LEFT JOIN sessions lead ON lead.session_key=r.lead_session_key AND lead.run_id=r.run_id
         LEFT JOIN launch_attempts l
           ON l.run_id=r.run_id AND l.review_id=r.review_id AND l.session_key=s.session_key
         LEFT JOIN report_heads h ON h.run_id=r.run_id
         LEFT JOIN report_versions v ON v.run_id=h.run_id AND v.receipt_id=h.receipt_id",
    )?;
    let mut rows = statement.query([selected])?;
    while let Some(row) = rows.next()? {
        let slot = usize::try_from(row.get::<_, i64>(0)?).map_err(|_| AppError::Internal)?;
        let signals = &mut items.get_mut(slot).ok_or(AppError::Internal)?.signals;
        signals.focused_provider = row
            .get::<_, Option<String>>(1)?
            .as_deref()
            .map(provider)
            .transpose()?;
        if row.get::<_, bool>(2)? {
            signals.focused_launch = Some(FocusedLaunch {
                observed_model: row.get(3)?,
                observed_effort: row.get(4)?,
                requested_model: row.get(5)?,
                requested_effort: row.get(6)?,
            });
        }
        signals.focused_report_reviewed_sha = row
            .get::<_, Option<String>>(7)?
            .as_deref()
            .map(parse)
            .transpose()?;
        signals.focused_activity = match row.get::<_, Option<String>>(8)?.as_deref() {
            Some("idle") => Some(HostPhase::Idle),
            Some("working") => Some(HostPhase::Working),
            Some("blocked") => Some(HostPhase::Blocked),
            Some("failed") => Some(HostPhase::Failed),
            None => None,
            _ => return Err(AppError::Internal.into()),
        };
        let mut providers =
            serde_json::from_str::<Vec<String>>(text(row, 9)?).map_err(|_| AppError::Internal)?;
        providers.sort_unstable();
        signals.registered_providers = providers
            .iter()
            .map(|value| provider(value))
            .collect::<Result<_, _>>()?;
        signals.focused_lead_name = row.get(11)?;
        signals.unread_non_reply_count = row
            .get::<_, i64>(10)?
            .try_into()
            .map_err(|_| AppError::Internal)?;
    }
    Ok(())
}
