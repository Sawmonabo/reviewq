use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::events::ResourceKey;
use reviewq_app::notifications::{DesktopClaim, DesktopOutcome, DesktopStore};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::queue::{parse, text};
use crate::{SqliteStore, StoreError};

const MAX_GROUP: u16 = 25;

impl DesktopStore for SqliteStore {
    fn recover_desktop_claims(&self) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(|tx| {
                let count = tx.sql.execute(
                    "UPDATE desktop_receipts SET state='uncertain', finished_at_ms=?1,
                       reason='service_interrupted' WHERE state='claimed'",
                    [now_ms()?],
                )?;
                if count != 0 {
                    tx.emit(&[ResourceKey::Notification(None)])?;
                }
                Ok(())
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn claim_desktop(&self) -> AppFuture<'_, Option<DesktopClaim>> {
        Box::pin(async move { self.write(|tx| claim(tx)).await.map_err(StoreError::app) })
    }

    fn desktop_still_unread(&self, claim: &DesktopClaim) -> AppFuture<'_, bool> {
        let id = claim.id.clone();
        let fence = claim.fence;
        Box::pin(async move {
            self.read(move |db| {
                let fence = i64::try_from(fence).map_err(|_| AppError::Internal)?;
                Ok(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM desktop_receipts d JOIN notifications n USING(notification_id)
                      WHERE d.receipt_id=?1 AND d.fence=?2 AND d.state='claimed'
                        AND n.read_at_ms IS NULL AND n.dismissed_at_ms IS NULL)",
                    params![id,fence], |row| row.get(0),
                )?)
            }).await.map_err(StoreError::app)
        })
    }

    fn finish_desktop(&self, claim: DesktopClaim, result: DesktopOutcome) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| finish(tx, claim, result))
                .await
                .map_err(StoreError::app)
        })
    }

    fn next_desktop_due(&self) -> AppFuture<'_, Option<i64>> {
        Box::pin(async move {
            self.read(|db| {
                Ok(db.query_row(
                    "SELECT d.due_at_ms FROM desktop_receipts d JOIN notifications n USING(notification_id)
                     WHERE d.state='failed' AND d.due_at_ms IS NOT NULL AND d.attempts<3 AND n.read_at_ms IS NULL
                       AND n.dismissed_at_ms IS NULL ORDER BY d.due_at_ms,d.receipt_id LIMIT 1",
                    [], |row| row.get(0),
                ).optional()?)
            }).await.map_err(StoreError::app)
        })
    }
}

fn claim(tx: &mut WriteTx<'_>) -> Result<Option<DesktopClaim>, StoreError> {
    let db = &tx.sql;
    let now = now_ms()?;
    let retry = db.query_row(
        "SELECT d.receipt_id FROM desktop_receipts d JOIN notifications n USING(notification_id)
         WHERE d.state='failed' AND d.due_at_ms<=?1 AND d.attempts<3 AND n.read_at_ms IS NULL
           AND n.dismissed_at_ms IS NULL ORDER BY d.due_at_ms,d.receipt_id LIMIT 1",
        [now], |row| row.get::<_, String>(0),
    ).optional()?;
    if let Some(retry) = retry {
        db.execute(
            "UPDATE desktop_receipts SET state='claimed',fence=fence+1,attempts=attempts+1,
                attempted_at_ms=?2,finished_at_ms=NULL,due_at_ms=NULL,reason=NULL WHERE receipt_id=?1",
            params![retry,now],
        )?;
        return read_claim(db, &retry).map(Some);
    }
    let next = db.query_row(
        "SELECT n.notification_id,n.review_id,n.desktop_episode FROM notifications n
         WHERE n.desktop_eligible=1 AND n.read_at_ms IS NULL AND n.dismissed_at_ms IS NULL
           AND n.notified_at_ms IS NULL AND NOT EXISTS (SELECT 1 FROM desktop_members m WHERE m.notification_id=n.notification_id)
           AND (n.desktop_episode IS NULL OR NOT EXISTS (SELECT 1 FROM desktop_receipts d WHERE d.episode_key=n.desktop_episode))
         ORDER BY n.ordinal LIMIT 1",
        [], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, Option<String>>(2)?)),
    ).optional()?;
    let Some((notification, review, episode)) = next else {
        return Ok(None);
    };
    let id = Uuid::new_v4().to_string();
    let episode_key = episode
        .clone()
        .unwrap_or_else(|| format!("notification:{notification}"));
    db.execute(
        "INSERT INTO desktop_receipts(receipt_id,episode_key,notification_id,state,fence,attempted_at_ms)
         VALUES(?1,?2,?3,'claimed',1,?4)",
        params![id,episode_key,notification,now],
    )?;
    db.execute(
        "INSERT INTO desktop_members(receipt_id,notification_id)
         SELECT ?1,n.notification_id FROM notifications n
         WHERE n.desktop_eligible=1 AND n.read_at_ms IS NULL AND n.dismissed_at_ms IS NULL
           AND n.notified_at_ms IS NULL AND NOT EXISTS (SELECT 1 FROM desktop_members m WHERE m.notification_id=n.notification_id)
           AND ((?2 IS NULL AND n.desktop_episode IS NULL AND n.review_id=?3) OR n.desktop_episode=?2)
         ORDER BY CASE WHEN n.notification_id=?4 THEN 0 ELSE 1 END, n.ordinal LIMIT ?5",
        params![id,episode,review,notification,MAX_GROUP],
    )?;
    read_claim(db, &id).map(Some)
}

fn read_claim(db: &Connection, id: &str) -> Result<DesktopClaim, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT d.fence,n.notification_id,n.review_id,n.run_id,n.session_key,n.kind,n.label,
           (SELECT count(*) FROM desktop_members WHERE receipt_id=d.receipt_id)
         FROM desktop_receipts d JOIN notifications n USING(notification_id) WHERE d.receipt_id=?1",
    )?;
    let mut rows = statement.query([id])?;
    let row = rows.next()?.ok_or(AppError::Internal)?;
    Ok(DesktopClaim {
        id: id.to_owned(),
        fence: u64::try_from(row.get::<_, i64>(0)?).map_err(|_| AppError::Internal)?,
        notification: parse(text(row, 1)?)?,
        review: parse(text(row, 2)?)?,
        run: row
            .get::<_, Option<String>>(3)?
            .as_deref()
            .map(parse)
            .transpose()?,
        session: row
            .get::<_, Option<String>>(4)?
            .as_deref()
            .map(parse)
            .transpose()?,
        kind: parse(text(row, 5)?)?,
        label: row.get(6)?,
        event_count: row.get(7)?,
    })
}

fn finish(
    tx: &mut WriteTx<'_>,
    claim: DesktopClaim,
    result: DesktopOutcome,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    let fence = i64::try_from(claim.fence).map_err(|_| AppError::Conflict)?;
    let attempts: i64 = db.query_row(
        "SELECT attempts FROM desktop_receipts WHERE receipt_id=?1 AND state='claimed' AND fence=?2",
        params![claim.id,fence], |row| row.get(0),
    ).optional()?.ok_or(AppError::Conflict)?;
    let time = now_ms()?;
    let (state, reason, due) = match result {
        DesktopOutcome::Delivered => ("delivered", None, None),
        DesktopOutcome::NotStarted(reason) => (
            "failed",
            Some(reason),
            (attempts < 3 && reason != "notification_no_longer_unread")
                .then_some(time.saturating_add(60_000 * attempts)),
        ),
        DesktopOutcome::Uncertain(reason) => ("uncertain", Some(reason), None),
    };
    db.execute(
        "UPDATE desktop_receipts SET state=?2,reason=?3,finished_at_ms=?4,due_at_ms=?5 WHERE receipt_id=?1",
        params![claim.id,state,reason,time,due],
    )?;
    if state == "delivered" {
        db.execute(
            "UPDATE notifications SET notified_at_ms=?2
             WHERE notification_id IN (SELECT notification_id FROM desktop_members WHERE receipt_id=?1)",
            params![claim.id,time],
        )?;
    }
    tx.emit(&[ResourceKey::Notification(None)])?;
    Ok(())
}
