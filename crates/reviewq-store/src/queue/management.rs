use reviewq_app::access::{Actor, AppError};
use reviewq_app::queue::{ManagementFilter, ManagementReviews, ManagementRuns, RunSummary};
use reviewq_domain::identity::ReviewId;
use rusqlite::{Connection, Row, params};

use super::{
    ITEM_COLUMNS, ReadScope, SCOPE_PREDICATE, allow_review, identity_for_review, parse, provider,
    queue_item, scope, settings_revision, text,
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
    let (scope_kind, scope_id) = match &access {
        ReadScope::Local => (0, None),
        ReadScope::Grant => (1, None),
        ReadScope::Review(review) => (2, Some(review.as_str())),
    };
    let idle_or_blocked = "EXISTS (SELECT 1 FROM sessions s JOIN runs USING(run_id)
        WHERE runs.review_id=r.review_id AND s.host_activity IN ('blocked','failed'))";
    let running = "EXISTS (SELECT 1 FROM sessions s JOIN runs USING(run_id)
        WHERE runs.review_id=r.review_id AND s.host_activity='working')";
    let registered = "EXISTS (SELECT 1 FROM runs WHERE runs.review_id=r.review_id)";
    let waiting = "EXISTS (SELECT 1 FROM review_decisions d WHERE d.review_id=r.review_id
        AND d.reviewed_sha=r.head_sha AND d.disposition IS NOT NULL)";
    let unhandled = "EXISTS (SELECT 1 FROM report_heads h JOIN runs USING(run_id)
        JOIN report_versions v ON v.run_id=h.run_id AND v.receipt_id=h.receipt_id
        WHERE runs.review_id=r.review_id AND v.coverage='full_mr'
          AND NOT EXISTS (
            SELECT 1 FROM review_handled_reports f WHERE f.review_id=r.review_id
            AND f.run_id=h.run_id AND f.attention_receipt_id=v.attention_receipt_id
          ))";
    let needs_input = format!("({idle_or_blocked} OR (NOT ({running}) AND {unhandled}))");
    let source = match filter {
        ManagementFilter::All => "1".to_owned(),
        ManagementFilter::Unstarted => format!("r.state='open' AND NOT ({registered})"),
        ManagementFilter::Archived => format!("r.state!='open' AND {registered}"),
        ManagementFilter::NeedsInput => {
            format!("r.state='open' AND {registered} AND {needs_input}")
        }
        ManagementFilter::Working => {
            format!("r.state='open' AND NOT ({idle_or_blocked}) AND {running}")
        }
        ManagementFilter::Waiting => format!(
            "r.state='open' AND {registered} AND {waiting}
                                             AND NOT ({running}) AND NOT ({needs_input})"
        ),
        ManagementFilter::AgentQuestions => "r.state='open' AND EXISTS (
            SELECT 1 FROM sessions s JOIN runs USING(run_id)
            WHERE runs.review_id=r.review_id AND s.host_questions=1)"
            .to_owned(),
    };
    let selected = format!("{SCOPE_PREDICATE} AND ({source})");
    let total_sql = format!("SELECT count(*) FROM reviews r WHERE {selected}");
    let total: i64 = tx.query_row(&total_sql, params![scope_kind, scope_id], |row| row.get(0))?;
    let page_sql = format!(
        "SELECT {ITEM_COLUMNS} FROM reviews r LEFT JOIN review_decisions d USING(review_id)
        WHERE {selected} ORDER BY r.updated_at_ms DESC,r.review_id DESC LIMIT ?3 OFFSET ?4"
    );
    let mut prepare = tx.prepare_cached(&page_sql)?;
    let mut rows = prepare.query(params![scope_kind, scope_id, limit, offset])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        items.push(queue_item(row)?);
    }
    Ok(ManagementReviews {
        items,
        total: total.try_into().map_err(|_| AppError::Internal)?,
        sync: read_sync(tx, settings_revision(tx)?)?,
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
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM reviews r LEFT JOIN review_decisions d USING(review_id) WHERE r.review_id=?1"
    );
    let mut statement = tx.prepare_cached(&sql)?;
    let mut found = statement.query([review.as_str()])?;
    let item = queue_item(found.next()?.ok_or(AppError::NotFound)?)?;
    drop(found);
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
    Ok(ManagementRuns {
        total: item.registered_lead_count,
        item,
        runs,
        sync: read_sync(tx, settings_revision(tx)?)?,
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
