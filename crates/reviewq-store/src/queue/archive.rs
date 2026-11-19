use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::queue::{
    ArchiveDate, ArchiveFilter, ArchiveItem, ArchiveMetadata, ArchivePage, ArchivePosition,
    ArchiveScope, ArchiveStore, ArchiveTarget, RunCard,
};
use reviewq_domain::identity::ProjectId;
use rusqlite::{Connection, named_params};

use super::workspace::{MR_SEARCH, RUN_IDS, read_scope, run_ids};
use super::{
    CACHED_ITEM_FROM, ITEM_COLUMNS, SCOPE_PREDICATE, cached, check_limit, make_context, parse,
    queue_item, text,
};
use crate::events::snapshot;
use crate::workspace::{scope, settings_revision};
use crate::{SqliteStore, StoreError};

impl ArchiveStore for SqliteStore {
    fn archive_scope(&self, actor: Actor) -> AppFuture<'_, Snapshot<ArchiveScope>> {
        Box::pin(async move {
            self.read(move |db| snapshot(db, archive_scope(db, actor)?))
                .await
                .map_err(StoreError::app)
        })
    }
    fn read_archive(
        &self,
        actor: Actor,
        captured: Arc<ArchiveScope>,
        filter: ArchiveFilter,
        metadata: Arc<ArchiveMetadata>,
        after: Option<ArchivePosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ArchivePage>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |db| {
                if &archive_scope(db, actor.clone())? != captured.as_ref() {
                    return Err(AppError::StaleRevision.into());
                }
                snapshot(db, archive(db, actor, filter, &metadata, after, limit)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn archive_scope(db: &Connection, actor: Actor) -> Result<ArchiveScope, StoreError> {
    let access = scope(db, &actor)?;
    let (code, owner) = read_scope(&access);
    let sql = format!(
        "SELECT r.review_id,r.project_id,
       (SELECT json_group_array(ticket_key) FROM (
          SELECT ticket_key FROM retained_review_tickets WHERE review_id=r.review_id
          UNION SELECT ticket_key FROM scoped_review_issues WHERE review_id=r.review_id
          ORDER BY ticket_key LIMIT 65))
       FROM reviews r WHERE {SCOPE_PREDICATE} AND r.state<>'open'
          AND EXISTS(SELECT 1 FROM configured_projects q WHERE (q.origin,q.project_id)=(r.origin,r.project_id))
          AND EXISTS(SELECT 1 FROM runs x WHERE x.review_id=r.review_id)
       ORDER BY r.review_id LIMIT 4097"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query(rusqlite::params![code, owner])?;
    let mut targets = Vec::new();
    let mut keys = HashSet::new();
    let mut limited = false;
    while let Some(row) = rows.next()? {
        let mut ticket_keys =
            serde_json::from_str::<Vec<String>>(text(row, 2)?).map_err(|_| AppError::Internal)?;
        if targets.len() >= 4096 {
            limited = true;
            break;
        }
        let complete = ticket_keys.len() <= 64;
        ticket_keys.truncate(64);
        keys.extend(ticket_keys.iter().cloned());
        if keys.len() > 8192 {
            limited = true;
            break;
        }
        targets.push(ArchiveTarget {
            review: parse(text(row, 0)?)?,
            project: ProjectId::try_from(row.get::<_, i64>(1)?).map_err(|_| AppError::Internal)?,
            keys: ticket_keys
                .iter()
                .map(|key| parse(key))
                .collect::<Result<BTreeSet<_>, _>>()?,
            complete,
        });
    }
    Ok(ArchiveScope {
        settings: settings_revision(db)?,
        targets,
        limited,
    })
}

fn archive(
    db: &Connection,
    actor: Actor,
    filter: ArchiveFilter,
    metadata: &ArchiveMetadata,
    after: Option<ArchivePosition>,
    limit: u16,
) -> Result<ArchivePage, StoreError> {
    let access = scope(db, &actor)?;
    let (code, owner) = read_scope(&access);
    let observation = cached::projection(db)?;
    let context = make_context(
        db,
        &actor,
        limit,
        after.as_ref().map(|row| &row.context),
        Some(observation),
    )?;
    let restrict = !filter.pairs.is_empty();
    let allowed = metadata
        .membership
        .iter()
        .filter(|(_, membership)| {
            filter
                .pairs
                .iter()
                .any(|pair| membership.pairs.contains(pair))
        })
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();
    let allowed = serde_json::to_string(&allowed).map_err(|_| AppError::Internal)?;
    let search = filter.search;
    let scope = SCOPE_PREDICATE
        .replace("?1", ":code")
        .replace("?2", ":owner");
    let sql=format!(
        "WITH active_archive AS MATERIALIZED(
           SELECT r.review_id FROM reviews r JOIN queue_projection f USING(review_id)
           WHERE ({scope}) AND r.state<>'open' AND f.run_id IS NOT NULL),
         selected AS MATERIALIZED(
           SELECT r.review_id FROM reviews r JOIN active_archive a USING(review_id)
           WHERE ({MR_SEARCH}) AND (:restrict=0 OR r.review_id IN (SELECT value FROM json_each(:allowed)))),
         times AS (
            SELECT d.review_id,d.reviewed_at_ms AS date,'decision' AS origin FROM review_decision_receipts d
            JOIN selected s USING(review_id)
            WHERE d.operation IN ('review_decision','terminal_review_decision') AND d.reviewed_at_ms IS NOT NULL
            UNION ALL
            SELECT x.review_id,v.captured_at_ms,'report' FROM selected s JOIN runs x USING(review_id)
            JOIN report_versions v USING(run_id) JOIN artifacts a USING(artifact_id)
            LEFT JOIN report_handoffs h ON h.run_id=v.run_id AND h.receipt_id=v.handoff_receipt_id
            WHERE a.sealed=1 AND v.coverage='full_mr'
              AND ((v.handoff_receipt_id IS NULL AND v.attention_receipt_id=v.receipt_id)
                   OR (a.complete=1 AND h.form='final' AND h.expected_receipt_id IS NULL))
            UNION ALL
            SELECT f.review_id,f.completed_at_ms,'followup' FROM followup_results f JOIN selected s USING(review_id)),
         ranked AS (
            SELECT review_id,date,origin,row_number() OVER(PARTITION BY review_id ORDER BY date DESC,origin) AS ordinal
            FROM times WHERE (:from IS NULL OR date>=:from) AND (:until IS NULL OR date<:until)),
         matches AS MATERIALIZED(
            SELECT s.review_id,t.date,t.origin FROM selected s LEFT JOIN ranked t ON t.review_id=s.review_id AND t.ordinal=1
            WHERE t.date IS NOT NULL OR (:from IS NULL AND :until IS NULL))"
    );
    let args = named_params! {":code":code,":owner":owner,":search":search,
    ":restrict":restrict,":allowed":allowed,":from":filter.from,":until":filter.until};
    let (total, matching) = db
        .prepare_cached(&format!(
            "{sql} SELECT (SELECT count(*) FROM active_archive),
        (SELECT count(*) FROM matches)"
        ))?
        .query_row(args, |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
    let (date, id) = after.as_ref().map_or((i64::MAX, ""), |pos| {
        (pos.latest.unwrap_or(i64::MIN), pos.review.as_str())
    });
    let wanted = limit + 1;
    let next_args = named_params! {":date":date,":id":id,":limit":wanted};
    let mut params = args.to_vec();
    params.extend_from_slice(next_args);
    let query=format!(
        "{sql}, page AS (
            SELECT * FROM matches WHERE coalesce(date,-9223372036854775808)<:date
              OR (coalesce(date,-9223372036854775808)=:date AND review_id>:id)
            ORDER BY coalesce(date,-9223372036854775808) DESC,review_id LIMIT :limit)
         SELECT {ITEM_COLUMNS},{RUN_IDS},p.date,p.origin {CACHED_ITEM_FROM} JOIN page p ON p.review_id=r.review_id
         ORDER BY coalesce(p.date,-9223372036854775808) DESC,r.review_id"
    );
    let mut statement = db.prepare_cached(&query)?;
    let mut rows = statement.query(params.as_slice())?;
    let mut items = Vec::new();
    let mut more = false;
    while let Some(row) = rows.next()? {
        if items.len() == usize::from(limit) {
            more = true;
            break;
        }
        let review = queue_item(row)?;
        let membership = metadata
            .membership
            .get(&review.review_id)
            .cloned()
            .unwrap_or_default();
        let date = match row
            .get_ref(35)?
            .as_str_or_null()
            .map_err(|_| AppError::Internal)?
        {
            Some("decision") => Some(ArchiveDate::Decision),
            Some("report") => Some(ArchiveDate::Report),
            Some("followup") => Some(ArchiveDate::Followup),
            None => None,
            _ => return Err(AppError::Internal.into()),
        };
        items.push(ArchiveItem {
            card: RunCard {
                review,
                runs: run_ids(text(row, 33)?)?,
            },
            latest: row.get(34)?,
            date,
            membership,
        });
    }
    let next = items.last().filter(|_| more).map(|row| ArchivePosition {
        context,
        latest: row.latest,
        review: row.card.review.review_id.clone(),
    });
    drop(rows);
    super::focus::hydrate(db, items.iter_mut().map(|item| &mut item.card.review))?;
    Ok(ArchivePage {
        items,
        next,
        total: total.try_into().map_err(|_| AppError::Internal)?,
        matching: matching.try_into().map_err(|_| AppError::Internal)?,
    })
}
