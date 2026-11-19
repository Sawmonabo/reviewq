use reviewq_app::access::{Actor, AppError};
use reviewq_app::queue::{ManagementFilter, ManagementReviews, ManagementRuns, RunSummary};
use reviewq_domain::identity::ReviewId;
use rusqlite::{Connection, Row, params};

use super::{
    CACHED_ITEM_FROM, ITEM_COLUMNS, ITEM_FROM, ReadScope, SCOPE_PREDICATE, allow_review, cached,
    identity_for_review, parse, provider, queue_item, scope, settings_revision, text,
};
use crate::StoreError;
use crate::refresh::read_sync;

pub(super) fn reviews(
    tx: &Connection,
    actor: Actor,
    filter: ManagementFilter,
    offset: u32,
    limit: u16,
) -> Result<ManagementReviews, StoreError> {
    let access = scope(tx, &actor)?;
    cached::projection(tx)?;
    let (scope_kind, scope_id) = match &access {
        ReadScope::Local => (0, None),
        ReadScope::Grant => (1, None),
        ReadScope::Review(review) => (2, Some(review.as_str())),
    };
    let source = match filter {
        ManagementFilter::All => "1",
        ManagementFilter::Unstarted => "r.state='open' AND f.run_id IS NULL",
        ManagementFilter::Archived => "f.stage_reason LIKE 'archived/%'",
        ManagementFilter::NeedsInput => "f.stage_reason LIKE 'needs_input/%'",
        ManagementFilter::Working => "f.stage_reason LIKE 'working/%'",
        ManagementFilter::Waiting => "f.stage_reason LIKE 'waiting/%'",
        ManagementFilter::AgentQuestions => "r.state='open' AND f.has_questions=1",
    };
    let selected = format!("{SCOPE_PREDICATE} AND ({source})");
    let total_sql = format!("SELECT count(*) {CACHED_ITEM_FROM} WHERE {selected}");
    let total: i64 = tx.query_row(&total_sql, params![scope_kind, scope_id], |row| row.get(0))?;
    let page_sql = format!(
        "SELECT {ITEM_COLUMNS} {CACHED_ITEM_FROM}
        WHERE {selected} ORDER BY CASE WHEN f.run_id IS NULL THEN r.updated_at_ms ELSE f.last_activity END DESC,r.review_id LIMIT ?3 OFFSET ?4"
    );
    let mut prepare = tx.prepare_cached(&page_sql)?;
    let mut rows = prepare.query(params![scope_kind, scope_id, limit, offset])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        items.push(queue_item(row)?);
    }
    super::focus::hydrate(tx, items.iter_mut())?;
    let settings = settings_revision(tx)?;
    Ok(ManagementReviews {
        items,
        total: total.try_into().map_err(|_| AppError::Internal)?,
        sync: read_sync(tx, settings)?,
        settings,
    })
}

pub(super) fn runs(
    tx: &Connection,
    actor: Actor,
    review: ReviewId,
    offset: u32,
    limit: u16,
) -> Result<ManagementRuns, StoreError> {
    let access = scope(tx, &actor)?;
    let mr = identity_for_review(tx, &review)?;
    allow_review(tx, access, &review, &mr)?;
    let sql = format!("SELECT {ITEM_COLUMNS} {ITEM_FROM} WHERE r.review_id=?1");
    let mut statement = tx.prepare_cached(&sql)?;
    let mut found = statement.query([review.as_str()])?;
    let mut item = queue_item(found.next()?.ok_or(AppError::NotFound)?)?;
    drop(found);
    super::focus::hydrate(tx, std::iter::once(&mut item))?;
    let mut statement = tx.prepare_cached(
        "SELECT r.run_id,r.started_at_ms,r.lead_session_key,s.provider
        FROM runs r JOIN sessions s ON s.run_id=r.run_id AND s.session_key=r.lead_session_key
        WHERE r.review_id=?1 ORDER BY r.started_at_ms DESC,r.run_id DESC LIMIT ?2 OFFSET ?3",
    )?;
    let mut rows = statement.query(params![review.as_str(), limit, offset])?;
    let mut runs = Vec::new();
    while let Some(row) = rows.next()? {
        runs.push(run(row)?);
    }
    let settings = settings_revision(tx)?;
    Ok(ManagementRuns {
        total: item.registered_lead_count,
        item,
        runs,
        sync: read_sync(tx, settings)?,
        settings,
    })
}

fn run(row: &Row<'_>) -> Result<RunSummary, StoreError> {
    Ok(RunSummary {
        run_id: parse(text(row, 0)?)?,
        started_at_ms: row.get(1)?,
        lead_session_key: parse(text(row, 2)?)?,
        provider: provider(text(row, 3)?)?,
    })
}
