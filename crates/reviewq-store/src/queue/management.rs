use reviewq_app::access::{Actor, AppError};
use reviewq_app::queue::{
    FindingMatch, ManagementFilter, ManagementReviews, ManagementRuns, QueueItem, RunSummary,
};
use reviewq_domain::identity::ReviewId;
use rusqlite::{Connection, Row, params};

use super::workspace::MR_SEARCH;
use super::{
    CACHED_ITEM_FROM, ITEM_COLUMNS, ITEM_FROM, ReadScope, SCOPE_PREDICATE, allow_review, cached,
    identity_for_review, parse, provider, queue_item, scope, settings_revision, text,
};
use crate::StoreError;
use crate::refresh::read_sync;

/// A review one of whose runs published a finding titled with `:search`, in any version, so a
/// finding a later version retired or retitled is found by the title a version showed. Only
/// `report_findings` rows count: they are what a submitted version shows, while an agent's
/// unsubmitted records stay in the ledger tables.
const FINDING_SEARCH: &str = "(:search<>'' AND EXISTS (SELECT 1 FROM runs u
      JOIN report_findings p ON p.run_id=u.run_id
      WHERE u.review_id=r.review_id AND instr(reviewq_fold(p.title),:search)>0))";

/// The finding `FINDING_SEARCH` matched for each review of `?1`, a JSON array of review IDs:
/// the newest version that showed a matching title first, then the lowest finding ID, and
/// whether that version is its run's current one.
const FINDING_MATCHES: &str =
    "SELECT review_id, run_id, lead_session_key, receipt_id, finding_id, title,
        current
    FROM (SELECT u.review_id, u.run_id, u.lead_session_key, p.receipt_id, p.finding_id, p.title,
            h.receipt_id IS NOT NULL AS current,
            row_number() OVER (PARTITION BY u.review_id ORDER BY v.published_at_ms DESC,
              v.receipt_id DESC, length(p.finding_id), p.finding_id) AS rank
          FROM json_each(?1) j JOIN runs u ON u.review_id=j.value
          JOIN report_findings p ON p.run_id=u.run_id
          JOIN report_versions v ON v.run_id=p.run_id AND v.receipt_id=p.receipt_id
          LEFT JOIN report_heads h ON h.run_id=p.run_id AND h.receipt_id=p.receipt_id
          WHERE instr(reviewq_fold(p.title),?2)>0)
    WHERE rank=1";

pub(super) fn reviews(
    tx: &Connection,
    actor: Actor,
    filter: ManagementFilter,
    search: String,
    offset: u32,
    limit: u16,
) -> Result<ManagementReviews, StoreError> {
    // The dashboard's queue search takes the same length and folds the same way.
    if search.chars().count() > 200 {
        return Err(AppError::InvalidRequest.into());
    }
    let search = search.to_lowercase();
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
    // `MR_SEARCH` names `:search` after `?1` and `?2`, so SQLite numbers it 3.
    let selected =
        format!("{SCOPE_PREDICATE} AND ({source}) AND ({MR_SEARCH} OR {FINDING_SEARCH})");
    let total_sql = format!("SELECT count(*) {CACHED_ITEM_FROM} WHERE {selected}");
    let total: i64 = tx.query_row(&total_sql, params![scope_kind, scope_id, search], |row| {
        row.get(0)
    })?;
    let page_sql = format!(
        "SELECT {ITEM_COLUMNS} {CACHED_ITEM_FROM}
        WHERE {selected} ORDER BY CASE WHEN f.run_id IS NULL THEN r.updated_at_ms ELSE f.last_activity END DESC,r.review_id LIMIT ?4 OFFSET ?5"
    );
    let mut prepare = tx.prepare_cached(&page_sql)?;
    let mut rows = prepare.query(params![scope_kind, scope_id, search, limit, offset])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        items.push(queue_item(row)?);
    }
    drop(rows);
    super::focus::hydrate(tx, items.iter_mut())?;
    let findings = if search.is_empty() {
        Vec::new()
    } else {
        finding_matches(tx, &items, &search)?
    };
    let settings = settings_revision(tx)?;
    Ok(ManagementReviews {
        items,
        findings,
        total: total.try_into().map_err(|_| AppError::Internal)?,
        sync: read_sync(tx, settings)?,
        settings,
    })
}

/// The finding each listed review matched by title, in one read for the whole page. The
/// reviews are the page's, already scoped, so the read adds no review to what the actor sees.
fn finding_matches(
    tx: &Connection,
    items: &[QueueItem],
    search: &str,
) -> Result<Vec<FindingMatch>, StoreError> {
    let ids: Vec<&str> = items.iter().map(|item| item.review_id.as_str()).collect();
    let ids = serde_json::to_string(&ids).map_err(|_| AppError::Internal)?;
    let mut statement = tx.prepare_cached(FINDING_MATCHES)?;
    let mut rows = statement.query(params![ids, search])?;
    let mut found = Vec::new();
    while let Some(row) = rows.next()? {
        found.push(FindingMatch {
            review_id: parse(text(row, 0)?)?,
            run_id: parse(text(row, 1)?)?,
            lead_session_key: parse(text(row, 2)?)?,
            receipt_id: parse(text(row, 3)?)?,
            finding_id: row.get(4)?,
            title: row.get(5)?,
            current: row.get(6)?,
        });
    }
    Ok(found)
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
        "SELECT r.run_id,r.started_at_ms,r.lead_session_key,s.provider,s.native_name
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
        lead_name: row.get(4)?,
    })
}
