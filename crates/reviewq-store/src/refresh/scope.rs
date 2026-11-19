use std::collections::HashSet;

use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::sources::{MAX_PUBLICATION_REVIEWS, RefreshAttempt, SourceObservation};
use reviewq_domain::identity::MrIdentity;
use rusqlite::Connection;

use crate::StoreError;
use crate::connection::WriteTx;
use crate::queue::{identity, save_observations};
use crate::workspace::advance_queue;

mod stage;

/// All persistent source writes happen after staging and the observation fence.
pub(super) fn publish(
    tx: &mut WriteTx<'_>,
    attempt: RefreshAttempt,
    source: SourceObservation,
) -> Result<Option<i64>, StoreError> {
    if source.reviews.len() > MAX_PUBLICATION_REVIEWS
        || source.owned.len() > MAX_PUBLICATION_REVIEWS
    {
        return Err(AppError::PayloadTooLarge.into());
    }
    stage::load(&tx.sql, &source)?;
    let excluded: bool = tx.sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM refresh_mrs q WHERE q.discovered=1 AND NOT EXISTS (
           SELECT 1 FROM configured_projects p WHERE (p.origin,p.project_id)=(q.origin,q.project_id)))",
        [],
        |row| row.get(0),
    )?;
    if excluded {
        return Err(AppError::NotFound.into());
    }
    let stale: bool = tx.sql.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM refresh_mrs q JOIN reviews r USING(origin,project_id,iid)
               WHERE r.observation_cursor>?1 AND {CHANGED_FACTS})"
        ),
        [attempt.id.as_sqlite_integer()],
        |row| row.get(0),
    )?;
    if stale {
        stage::clear(&tx.sql)?;
        return Ok(None);
    }
    let changed = changed_sources(&tx.sql)?;
    let updated = source
        .reviews
        .into_values()
        .map(|(review, _)| review)
        .chain(source.owned)
        .filter(|review| changed.contains(&review.identity))
        .collect::<Vec<_>>();
    if !updated.is_empty() {
        save_observations(tx, &updated)?;
    }
    crate::queue::tracking::retain_current_tickets(&tx.sql)?;
    let discoveries = stage::discover(&tx.sql)?;
    let scope = stage::replace_scope(&tx.sql)?;
    if discoveries || scope {
        if updated.is_empty() {
            advance_queue(&tx.sql)?;
        }
        if scope {
            crate::queue::tracking::retain_current_tickets(&tx.sql)?;
        }
        tx.emit(if scope {
            &[ResourceKey::Queue, ResourceKey::Ticket(None)]
        } else {
            &[ResourceKey::Queue]
        })?;
    }
    let total = tx.sql.query_row(
        "SELECT (SELECT count(*) FROM review_discoveries)
            + (SELECT count(DISTINCT i.ticket_key) FROM scoped_issues i
               WHERE NOT EXISTS (SELECT 1 FROM scoped_review_issues a WHERE a.ticket_key=i.ticket_key))",
        [],
        |row| row.get(0),
    )?;
    stage::clear(&tx.sql)?;
    Ok(Some(total))
}

const CHANGED_FACTS: &str = "(q.project_path,q.title,q.head_sha,q.state,q.updated_at_ms)
            IS DISTINCT FROM (r.project_path,r.title,r.head_sha,r.state,r.updated_at_ms)";

fn changed_sources(db: &Connection) -> Result<HashSet<MrIdentity>, StoreError> {
    let statement = format!(
        "SELECT q.origin,q.project_id,q.iid FROM refresh_mrs q
         LEFT JOIN reviews r USING(origin,project_id,iid)
         WHERE r.review_id IS NULL OR (q.updated_at_ms>=r.updated_at_ms AND {CHANGED_FACTS})"
    );
    let mut query = db.prepare_cached(&statement)?;
    let mut rows = query.query([])?;
    let mut result = HashSet::new();
    while let Some(row) = rows.next()? {
        result.insert(identity(row, 0)?);
    }
    Ok(result)
}
