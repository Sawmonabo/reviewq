//! Original launch and accepted report evidence for a bounded selected page.

use reviewq_app::access::AppError;
use reviewq_app::queue::{FocusedLaunch, QueueItem};
use rusqlite::Connection;

use super::{parse, provider, text};
use crate::StoreError;

pub(super) fn hydrate<'a>(
    db: &Connection,
    items: impl Iterator<Item = &'a mut QueueItem>,
) -> Result<(), StoreError> {
    let mut items: Vec<_> = items.filter(|item| item.signals.focus.is_some()).collect();
    if items.is_empty() {
        return Ok(());
    }
    let keys = items
        .iter()
        .enumerate()
        .filter_map(|(slot, item)| {
            Some((
                slot,
                item.review_id.as_str(),
                item.signals.run.as_ref()?.as_str(),
                item.signals.focus.as_ref()?.as_str(),
            ))
        })
        .collect::<Vec<_>>();
    let selected = serde_json::to_string(&keys).map_err(|_| AppError::Internal)?;
    let mut statement = db.prepare_cached(
        "SELECT p.value->>0,s.provider,l.run_id IS NOT NULL,l.observed_model,l.observed_effort,
                l.requested_model,l.requested_effort,v.reviewed_sha
         FROM json_each(?1) p
         JOIN runs r ON r.run_id=(p.value->>2) AND r.review_id=(p.value->>1)
         JOIN sessions s ON s.session_key=(p.value->>3) AND s.run_id=r.run_id
         LEFT JOIN launch_attempts l
           ON l.run_id=r.run_id AND l.review_id=r.review_id AND l.session_key=r.lead_session_key
         LEFT JOIN report_heads h ON h.run_id=r.run_id
         LEFT JOIN report_versions v ON v.run_id=h.run_id AND v.receipt_id=h.receipt_id",
    )?;
    let mut rows = statement.query([selected])?;
    while let Some(row) = rows.next()? {
        let slot = usize::try_from(row.get::<_, i64>(0)?).map_err(|_| AppError::Internal)?;
        let signals = &mut items.get_mut(slot).ok_or(AppError::Internal)?.signals;
        signals.focused_provider = Some(provider(text(row, 1)?)?);
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
    }
    Ok(())
}
