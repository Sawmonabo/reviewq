//! Inbox occurrence admission, scoped projection and exact-ID acknowledgement.

use std::collections::HashSet;
mod desktop;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::notifications::{
    InboxChange, InboxPage, InboxPosition, InboxStore, InboxView, MAX_INBOX_PAGE,
    MAX_INBOX_SELECTION, Notification, NotificationAction, UnreadCounts,
};
use reviewq_domain::identity::{NotificationId, Revision};
use rusqlite::{Connection, Row, params};

use crate::events::{now_ms, snapshot};
use crate::queue::{parse, text};
use crate::workspace::{ReadScope, actor_key, scope, settings_revision};
use crate::{SqliteStore, StoreError};

impl InboxStore for SqliteStore {
    fn read_inbox(
        &self,
        actor: Actor,
        view: InboxView,
        after: Option<InboxPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<InboxPage>> {
        Box::pin(async move {
            if !(1..=MAX_INBOX_PAGE).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| snapshot(tx, page(tx, actor, view, after, limit)?))
                .await
                .map_err(StoreError::app)
        })
    }

    fn assign_notifications(
        &self,
        actor: Actor,
        action: NotificationAction,
        ids: Vec<NotificationId>,
    ) -> AppFuture<'_, Snapshot<InboxChange>> {
        Box::pin(async move {
            if ids.is_empty() || ids.len() > MAX_INBOX_SELECTION {
                return Err(AppError::InvalidRequest);
            }
            let mut deduplicated = HashSet::new();
            let ids: Vec<_> = ids
                .into_iter()
                .filter(|id| deduplicated.insert(id.clone()))
                .collect();
            let selection =
                serde_json::to_string(&ids.iter().map(|id| id.as_str()).collect::<Vec<_>>())
                    .map_err(|_| AppError::Internal)?;
            self.write(move |tx| {
                let (scope_code, scope_id) = define_scope(scope(&tx.sql, &actor)?);
                let authorized_sql = format!(
                    "SELECT count(*) FROM json_each(?3) selected
                     JOIN notifications n ON n.notification_id=selected.value
                     JOIN reviews r ON r.review_id=n.review_id WHERE {SCOPE_PREDICATE}"
                );
                let approved: i64 = tx.sql.prepare_cached(&authorized_sql)?
                    .query_row(params![scope_code, scope_id, selection], |row| row.get(0))?;
                if approved != ids.len() as i64 {
                    return Err(AppError::NotFound.into());
                }
                let affected = assign(&tx.sql, action, ids, &selection)?;
                if !affected.is_empty() {
                    tx.sql.execute(
                        "UPDATE service_state SET inbox_revision=inbox_revision+1 WHERE singleton=1", [],
                    )?;
                    let keys = if affected.len() <= 31 {
                        affected.iter().map(|id| ResourceKey::Notification(Some(id.clone())))
                            .collect::<Vec<_>>()
                    } else {
                        vec![ResourceKey::Notification(None)]
                    };
                    tx.emit(&keys)?;
                }
                let counts = counts(&tx.sql, scope_code, &scope_id, i64::MAX)?;
                snapshot(&tx.sql, InboxChange { affected, counts })
            }).await.map_err(StoreError::app)
        })
    }
}

const SCOPE_PREDICATE: &str = "(?1=0 OR (?1=1 AND EXISTS(
        SELECT 1 FROM configured_projects p WHERE p.origin=r.origin AND p.project_id=r.project_id
      )) OR (?1=2 AND n.review_id=?2))";

const JOIN_SCOPE: &str = "FROM notifications n JOIN reviews r ON r.review_id=n.review_id
    WHERE n.ordinal<=?3 AND n.dismissed_at_ms IS NULL";

fn assign(
    db: &Connection,
    action: NotificationAction,
    ids: Vec<NotificationId>,
    selection: &str,
) -> Result<Vec<NotificationId>, StoreError> {
    let (field, only_if) = match action {
        NotificationAction::Read => ("read_at_ms=?2", "AND read_at_ms IS NULL"),
        NotificationAction::Unread => ("read_at_ms=NULL", "AND read_at_ms IS NOT NULL"),
        NotificationAction::Dismiss => ("dismissed_at_ms=?2", ""),
    };
    let sql = format!(
        "UPDATE notifications SET {field} WHERE dismissed_at_ms IS NULL {only_if}
         AND notification_id IN (SELECT value FROM json_each(?1)) RETURNING notification_id"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = match action {
        NotificationAction::Unread => statement.query([selection])?,
        _ => statement.query(params![selection, now_ms()?])?,
    };
    let mut updates = HashSet::<NotificationId>::new();
    while let Some(row) = rows.next()? {
        updates.insert(parse(text(row, 0)?)?);
    }
    Ok(ids.into_iter().filter(|id| updates.contains(id)).collect())
}

fn define_scope(scope: ReadScope) -> (i64, String) {
    match scope {
        ReadScope::Local => (0, String::new()),
        ReadScope::Grant => (1, String::new()),
        ReadScope::Review(id) => (2, id.to_string()),
    }
}

fn page(
    db: &Connection,
    actor: Actor,
    view: InboxView,
    after: Option<InboxPosition>,
    limit: u16,
) -> Result<InboxPage, StoreError> {
    use sha2::{Digest, Sha256};

    let (scope_code, scope_id) = define_scope(scope(db, &actor)?);
    let mut digest = Sha256::new();
    digest.update(actor_key(&actor));
    digest.update(actor.grant_epoch().map_or(0, Revision::get).to_be_bytes());
    digest.update(settings_revision(db)?.get().to_be_bytes());
    let scope_key: [u8; 32] = digest.finalize().into();
    let revision = Revision::try_from(db.query_row(
        "SELECT inbox_revision FROM service_state WHERE singleton=1",
        [],
        |row| row.get::<_, i64>(0),
    )?)
    .map_err(|_| AppError::Internal)?;
    let (watermark, time, source, id) = if let Some(position) = &after {
        if position.scope != scope_key
            || position.revision != revision
            || position.limit != limit
            || position.view != view
        {
            return Err(AppError::CursorExpired.into());
        }
        (
            position.watermark,
            Some(position.occurred_at_ms),
            position.source.to_string(),
            position.id.as_str(),
        )
    } else {
        let watermark = db.query_row(
            "SELECT coalesce(max(ordinal), 0) FROM notifications",
            [],
            |row| row.get(0),
        )?;
        (watermark, None, String::new(), "")
    };
    let sql = format!(
        "SELECT n.notification_id, n.source, n.kind, n.review_id, n.run_id, n.session_key,
             n.label, n.occurred_at_ms, n.read_at_ms, n.notified_at_ms, n.event_ref,
             (SELECT d.state FROM desktop_members m JOIN desktop_receipts d USING(receipt_id)
               WHERE m.notification_id=n.notification_id),
             (SELECT d.reason FROM desktop_members m JOIN desktop_receipts d USING(receipt_id)
               WHERE m.notification_id=n.notification_id)
         {JOIN_SCOPE} AND {SCOPE_PREDICATE}
           AND (?4 OR n.read_at_ms IS NULL)
           AND (?5 IS NULL OR n.occurred_at_ms<?5 OR
             (n.occurred_at_ms=?5 AND (n.source>?6 OR (n.source=?6 AND n.notification_id<?7))))
         ORDER BY n.occurred_at_ms DESC, n.source ASC, n.notification_id DESC LIMIT ?8"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query(params![
        scope_code,
        scope_id,
        watermark,
        view == InboxView::All,
        time,
        source,
        id,
        limit + 1,
    ])?;
    let mut items = Vec::with_capacity(usize::from(limit));
    let mut next = None;
    while let Some(row) = rows.next()? {
        if items.len() == usize::from(limit) {
            next = items.last().map(|item: &Notification| InboxPosition {
                scope: scope_key,
                limit,
                view,
                watermark,
                revision,
                occurred_at_ms: item.occurred_at_ms,
                source: item.source,
                id: item.id.clone(),
            });
            break;
        }
        items.push(decode(row)?);
    }
    Ok(InboxPage {
        items,
        next,
        counts: counts(db, scope_code, &scope_id, watermark)?,
    })
}

fn decode(row: &Row<'_>) -> Result<Notification, StoreError> {
    Ok(Notification {
        id: parse(text(row, 0)?)?,
        source: parse(text(row, 1)?)?,
        kind: parse(text(row, 2)?)?,
        review: parse(text(row, 3)?)?,
        run: row
            .get::<_, Option<String>>(4)?
            .as_deref()
            .map(parse)
            .transpose()?,
        session: row
            .get::<_, Option<String>>(5)?
            .as_deref()
            .map(parse)
            .transpose()?,
        label: row.get(6)?,
        occurred_at_ms: row.get(7)?,
        read_at_ms: row.get(8)?,
        notified_at_ms: row.get(9)?,
        event_ref: row.get(10)?,
        desktop_status: row.get(11)?,
        desktop_reason: row.get(12)?,
    })
}

fn counts(
    db: &Connection,
    scope: i64,
    scope_id: &str,
    watermark: i64,
) -> Result<UnreadCounts, StoreError> {
    let sql = format!(
        "SELECT count(*), count(DISTINCT n.review_id) {JOIN_SCOPE} AND {SCOPE_PREDICATE} AND n.read_at_ms IS NULL"
    );
    let (events, mrs): (i64, i64) =
        db.query_row(&sql, params![scope, scope_id, watermark], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    Ok(UnreadCounts {
        event_count: u64::try_from(events).map_err(|_| AppError::Internal)?,
        mr_count: u64::try_from(mrs).map_err(|_| AppError::Internal)?,
    })
}

/// Saves one stable occurrence in the transaction that observed it.
pub(crate) fn insert_notification(
    db: &Connection,
    input: reviewq_app::notifications::Observation,
) -> Result<Option<NotificationId>, StoreError> {
    let id = uuid::Uuid::new_v4().to_string();
    let changed = db.execute(
        "INSERT INTO notifications(notification_id, source, occurrence_id, kind, review_id, run_id,
                                    session_key, event_ref, label, occurred_at_ms, desktop_eligible, desktop_episode)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11)
         ON CONFLICT(source, occurrence_id) DO NOTHING",
        params![
            id,
            input.source.to_string(),
            input.occurrence_id,
            input.kind.to_string(),
            input.review.as_str(),
            input.run.as_ref().map(|v| v.as_str()),
            input.session.as_ref().map(|v| v.as_str()),
            input.event_ref,
            input.label,
            input.occurred_at_ms, input.desktop_episode,
        ],
    )?;
    (changed == 1).then(|| parse(&id)).transpose()
}
