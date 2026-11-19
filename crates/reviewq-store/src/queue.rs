//! Bounded review observations, scoped projections, and exact review selection.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::str::FromStr;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::queue::{
    MAX_QUEUE_PAGE, ManagementFilter, ManagementReviews, ManagementRuns, MergeRequestState,
    PageContext, QueueItem, QueuePage, QueuePosition, QueueStore, ReviewLookup, ReviewOverview,
    ReviewSource, RunDetail, RunPosition, RunSummary, SelectedLead, SessionPosition,
    SessionSummary,
};
use reviewq_domain::identity::{
    GitLabOrigin, MergeRequestIid, MrIdentity, ProjectId, ReviewId, RunId,
};
use reviewq_domain::session::{
    AbsolutePath, HostProvider, LeadTarget, NativeBinding, NativeIdentity, SessionTarget,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use crate::connection::WriteTx;
use crate::decisions;
use crate::events::snapshot;
use crate::workspace::{
    ReadScope, actor_key, advance_queue, allow_review, operator, queue_revision, scope,
    settings_revision,
};
use crate::{SqliteStore, StoreError};

mod management;

const ITEM_COLUMNS: &str =
    "r.review_id, r.origin, r.project_id, r.iid, r.project_path, r.title, r.head_sha, r.state, r.updated_at_ms,
     (SELECT count(*) FROM runs WHERE runs.review_id=r.review_id),
     coalesce(d.revision, 0), d.disposition, d.reviewed_sha, d.reviewed_at_ms,
     EXISTS (SELECT 1 FROM notifications n WHERE n.review_id=r.review_id AND n.source='gitlab'
             AND n.kind IN ('reply', 'edited_reply', 'resolved', 'reopened', 'approval_withdrawn')
             AND n.ordinal > coalesce((SELECT f.source_ordinal FROM review_handled_frontiers f
                                        WHERE f.review_id=r.review_id), 0)),
     EXISTS (SELECT 1 FROM notifications n WHERE n.review_id=r.review_id AND n.source IN ('codex', 'claude')
             AND n.run_id IS NOT NULL AND n.session_key IS NOT NULL
             AND n.ordinal > coalesce((SELECT h.ordinal FROM review_handled_sessions h
                 WHERE h.review_id=r.review_id AND h.run_id=n.run_id AND h.session_key=n.session_key), 0)),
     EXISTS (SELECT 1 FROM runs run JOIN report_heads h USING(run_id)
             JOIN report_versions v USING(run_id, receipt_id)
             WHERE run.review_id=r.review_id AND NOT EXISTS (
               SELECT 1 FROM review_handled_reports handled WHERE handled.review_id=r.review_id
                 AND handled.run_id=run.run_id AND handled.attention_receipt_id=v.attention_receipt_id
             ))";
const SCOPE_PREDICATE: &str =
    "(((?1 = 0 OR ?1 = 1)
       AND EXISTS (SELECT 1 FROM configured_projects c WHERE c.origin=r.origin AND c.project_id=r.project_id)
       AND (EXISTS (SELECT 1 FROM review_discoveries d WHERE d.review_id=r.review_id)
            OR EXISTS (SELECT 1 FROM runs WHERE runs.review_id=r.review_id)
            OR EXISTS (SELECT 1 FROM review_decisions WHERE review_id=r.review_id)
            OR EXISTS (SELECT 1 FROM notifications WHERE review_id=r.review_id)))
      OR (?1 = 0 AND (EXISTS (SELECT 1 FROM runs WHERE runs.review_id=r.review_id)
            OR EXISTS (SELECT 1 FROM review_decisions WHERE review_id=r.review_id)
            OR EXISTS (SELECT 1 FROM notifications WHERE review_id=r.review_id)))
      OR (?1 = 2 AND r.review_id=?2))";

impl QueueStore for SqliteStore {
    fn observe_reviews(
        &self,
        actor: Actor,
        source: Vec<ReviewSource>,
    ) -> AppFuture<'_, Snapshot<Vec<ReviewId>>> {
        Box::pin(async move {
            self.write(move |tx| {
                operator(&actor)?;
                if source.is_empty() || source.len() > usize::from(MAX_QUEUE_PAGE) {
                    return Err(AppError::InvalidRequest.into());
                }
                let results = save_observations(tx, &source)?;
                if mark_discovered(&tx.sql, &results)? {
                    advance_queue(&tx.sql)?;
                    tx.emit(&[ResourceKey::Queue])?;
                }
                snapshot(&tx.sql, results)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_queue(
        &self,
        actor: Actor,
        after: Option<QueuePosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<QueuePage>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |tx| snapshot(tx, queue(tx, actor, after, limit)?))
                .await
                .map_err(StoreError::app)
        })
    }

    fn read_review(
        &self,
        actor: Actor,
        review_id: ReviewId,
        after: Option<RunPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ReviewOverview>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |tx| snapshot(tx, overview(tx, actor, review_id, after, limit)?))
                .await
                .map_err(StoreError::app)
        })
    }

    fn read_run(
        &self,
        actor: Actor,
        run_id: RunId,
        after: Option<SessionPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<RunDetail>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |tx| snapshot(tx, run_detail(tx, actor, run_id, after, limit)?))
                .await
                .map_err(StoreError::app)
        })
    }
}

pub(crate) fn mark_discovered(
    connection: &Connection,
    reviews: &[ReviewId],
) -> Result<bool, StoreError> {
    let mut insert = connection.prepare_cached(
        "INSERT INTO review_discoveries(review_id) VALUES(?1) ON CONFLICT DO NOTHING",
    )?;
    let mut changed = false;
    for review in reviews {
        changed |= insert.execute([review.as_str()])? != 0;
    }
    Ok(changed)
}

pub(crate) fn save_observations(
    tx: &mut WriteTx<'_>,
    source: &[ReviewSource],
) -> Result<Vec<ReviewId>, StoreError> {
    let mut results = Vec::with_capacity(source.len());
    let mut changed = Vec::new();
    for item in source {
        let (id, written) = save_review(&tx.sql, item, true)?;
        if written {
            changed.push(ResourceKey::Review(Some(id.clone())));
        }
        results.push(id);
    }
    if !changed.is_empty() {
        advance_queue(&tx.sql)?;
        for group in changed.chunks(reviewq_app::events::MAX_INVALIDATION_KEYS - 1) {
            let mut keys = vec![ResourceKey::Queue];
            keys.extend_from_slice(group);
            tx.emit(&keys)?;
        }
    }
    Ok(results)
}

impl ReviewLookup for SqliteStore {
    fn management_reviews(
        &self,
        actor: Actor,
        filter: ManagementFilter,
        offset: u32,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ManagementReviews>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |tx| {
                snapshot(tx, management::reviews(tx, actor, filter, offset, limit)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn management_runs(
        &self,
        actor: Actor,
        review: ReviewId,
        offset: u32,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ManagementRuns>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |tx| snapshot(tx, management::runs(tx, actor, review, offset, limit)?))
                .await
                .map_err(StoreError::app)
        })
    }

    fn review_by_mr(&self, actor: Actor, mr: MrIdentity) -> AppFuture<'_, ReviewId> {
        Box::pin(async move {
            self.read(move |connection| {
                let access = scope(connection, &actor)?;
                let id = find_review_id(connection, &mr)?.ok_or(AppError::NotFound)?;
                allow_review(connection, access, &id, &mr)?;
                Ok(id)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn run_for_session(
        &self,
        actor: Actor,
        review: ReviewId,
        session: reviewq_domain::identity::SessionKey,
    ) -> AppFuture<'_, RunId> {
        Box::pin(async move {
            self.read(move |connection| {
                let access = scope(connection, &actor)?;
                let mr = identity_for_review(connection, &review)?;
                allow_review(connection, access, &review, &mr)?;
                let id = connection.query_row(
                    "SELECT r.run_id FROM runs r JOIN sessions s USING(run_id) WHERE r.review_id=?1 AND s.session_key=?2",
                    params![review.as_str(), session.as_str()],
                    |row| row.get::<_, String>(0),
                ).optional()?.ok_or(AppError::NotFound)?;
                parse(&id)
            }).await.map_err(StoreError::app)
        })
    }

    fn resolve_selection(
        &self,
        actor: Actor,
        review_id: ReviewId,
        run_id: Option<RunId>,
    ) -> AppFuture<'_, Snapshot<SelectedLead>> {
        Box::pin(async move {
            self.read(move |tx| {
                let access = scope(tx, &actor)?;
                let identity = identity_for_review(tx, &review_id)?;
                allow_review(tx, access, &review_id, &identity)?;
                let chosen = match run_id {
                    Some(id) => {
                        let lead = lead_for_run(tx, &id)?.ok_or(AppError::NotFound)?;
                        if lead.review_id() != &review_id {
                            return Err(AppError::NotFound.into());
                        }
                        Some(lead)
                    }
                    None => {
                        let latest = tx.query_row(
                            "SELECT run_id FROM runs WHERE review_id=?1 ORDER BY started_at_ms DESC, run_id DESC LIMIT 1",
                            [review_id.as_str()], |row| row.get::<_, String>(0),
                        ).optional()?;
                        latest.map(|id| lead_for_run(tx, &parse(&id)?)).transpose()?.flatten()
                    }
                };
                snapshot(tx, SelectedLead::new(review_id, chosen)?)
            }).await.map_err(StoreError::app)
        })
    }
}

fn check_limit(limit: u16) -> Result<(), AppError> {
    if !(1..=MAX_QUEUE_PAGE).contains(&limit) {
        return Err(AppError::InvalidRequest);
    }
    Ok(())
}

fn page_context(
    connection: &Connection,
    actor: &Actor,
    limit: u16,
    previous: Option<&PageContext>,
) -> Result<PageContext, StoreError> {
    let context = PageContext {
        generation: queue_revision(connection)?,
        authority: actor_key(actor),
        limit,
    };
    if previous.is_some_and(|previous| previous != &context) {
        return Err(AppError::CursorExpired.into());
    }
    Ok(context)
}

fn queue(
    connection: &Connection,
    actor: Actor,
    after: Option<QueuePosition>,
    limit: u16,
) -> Result<QueuePage, StoreError> {
    let access = scope(connection, &actor)?;
    let context = page_context(
        connection,
        &actor,
        limit,
        after.as_ref().map(|row| &row.context),
    )?;
    let (scope_code, scope_id) = match &access {
        ReadScope::Local => (0, None),
        ReadScope::Grant => (1, None),
        ReadScope::Review(review) => (2, Some(review.as_str())),
    };
    let (key_time, key_id) = after.as_ref().map_or((i64::MAX, "~"), |position| {
        (position.updated_at_ms, position.review_id.as_str())
    });
    let total_sql = format!("SELECT count(*) FROM reviews AS r WHERE {SCOPE_PREDICATE}");
    let page_sql = format!(
        "SELECT {ITEM_COLUMNS} FROM reviews AS r LEFT JOIN review_decisions AS d USING (review_id) WHERE {SCOPE_PREDICATE}
         AND (r.updated_at_ms, r.review_id) < (?3, ?4) ORDER BY r.updated_at_ms DESC, r.review_id DESC LIMIT ?5"
    );
    let total: i64 =
        connection.query_row(&total_sql, params![scope_code, scope_id], |row| row.get(0))?;
    let total = u64::try_from(total).map_err(|_| AppError::Internal)?;
    let mut statement = connection.prepare_cached(&page_sql)?;
    let mut rows = statement.query(params![scope_code, scope_id, key_time, key_id, limit + 1])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        items.push(queue_item(row)?);
    }
    let next = if items.len() > usize::from(limit) {
        items.pop();
        items.last().map(|item| QueuePosition {
            context,
            updated_at_ms: item.source.updated_at_ms,
            review_id: item.review_id.clone(),
        })
    } else {
        None
    };
    let settings_revision = settings_revision(connection)?;
    Ok(QueuePage {
        items,
        next,
        total,
        settings_revision,
        sync: crate::refresh::read_sync(connection, settings_revision)?,
    })
}

fn overview(
    connection: &Connection,
    actor: Actor,
    review_id: ReviewId,
    after: Option<RunPosition>,
    limit: u16,
) -> Result<ReviewOverview, StoreError> {
    let access = scope(connection, &actor)?;
    let identity = identity_for_review(connection, &review_id)?;
    allow_review(connection, access, &review_id, &identity)?;
    let context = page_context(
        connection,
        &actor,
        limit,
        after.as_ref().map(|row| &row.context),
    )?;
    if after.as_ref().is_some_and(|row| row.review_id != review_id) {
        return Err(AppError::CursorExpired.into());
    }
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {ITEM_COLUMNS} FROM reviews AS r LEFT JOIN review_decisions AS d USING (review_id) WHERE r.review_id=?1"
    ))?;
    let mut rows = statement.query([review_id.as_str()])?;
    let item = queue_item(rows.next()?.ok_or(AppError::NotFound)?)?;

    let (time, id) = after.as_ref().map_or((i64::MAX, "~"), |position| {
        (position.started_at_ms, position.run_id.as_str())
    });
    let mut statement = connection.prepare_cached(
        "SELECT r.run_id, r.started_at_ms, r.lead_session_key, s.provider
         FROM runs r JOIN sessions s ON s.run_id=r.run_id AND s.session_key=r.lead_session_key
         WHERE r.review_id=?1 AND (r.started_at_ms, r.run_id) < (?2, ?3)
         ORDER BY r.started_at_ms DESC, r.run_id DESC LIMIT ?4",
    )?;
    let mut rows = statement.query(params![review_id.as_str(), time, id, limit + 1])?;
    let mut runs = Vec::new();
    while let Some(row) = rows.next()? {
        runs.push(RunSummary {
            started_at_ms: row.get(1)?,
            run_id: parse(text(row, 0)?)?,
            lead_session_key: parse(text(row, 2)?)?,
            provider: provider(text(row, 3)?)?,
        });
    }
    let next_run = if runs.len() > usize::from(limit) {
        runs.pop();
        runs.last().map(|run| RunPosition {
            context,
            review_id: review_id.clone(),
            started_at_ms: run.started_at_ms,
            run_id: run.run_id.clone(),
        })
    } else {
        None
    };
    Ok(ReviewOverview {
        item,
        settings_revision: settings_revision(connection)?,
        frontier: decisions::capture_frontier(connection, &review_id, &runs)?,
        runs,
        next_run,
    })
}

fn run_detail(
    connection: &Connection,
    actor: Actor,
    run_id: RunId,
    after: Option<SessionPosition>,
    limit: u16,
) -> Result<RunDetail, StoreError> {
    let access = scope(connection, &actor)?;
    let lead = lead_for_run(connection, &run_id)?.ok_or(AppError::NotFound)?;
    let source = identity_for_review(connection, lead.review_id())?;
    allow_review(connection, access, lead.review_id(), &source)?;
    let context = page_context(
        connection,
        &actor,
        limit,
        after.as_ref().map(|row| &row.context),
    )?;
    if after.as_ref().is_some_and(|row| row.run_id != run_id) {
        return Err(AppError::CursorExpired.into());
    }
    let mut statement = connection.prepare_cached(
        "SELECT session_key, parent_session_key, provider FROM sessions
         WHERE run_id=?1 AND session_key>?2 ORDER BY session_key LIMIT ?3",
    )?;
    let mut rows = statement.query(params![
        run_id.as_str(),
        after.as_ref().map_or("", |row| row.session_key.as_str()),
        limit + 1
    ])?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next()? {
        let parent = row
            .get::<_, Option<String>>(1)?
            .map(|id| parse(&id))
            .transpose()?;
        sessions.push(SessionSummary {
            session_key: parse(text(row, 0)?)?,
            parent,
            provider: provider(text(row, 2)?)?,
        });
    }
    let next_session = if sessions.len() > usize::from(limit) {
        sessions.pop();
        sessions.last().map(|session| SessionPosition {
            context,
            run_id,
            session_key: session.session_key.clone(),
        })
    } else {
        None
    };
    Ok(RunDetail {
        lead,
        sessions,
        next_session,
    })
}

pub(crate) fn lead_for_run(
    connection: &Connection,
    run_id: &RunId,
) -> Result<Option<LeadTarget>, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT r.review_id, r.lead_session_key, s.provider, s.profile, s.native_id, s.cwd
         FROM runs r JOIN sessions s ON s.run_id=r.run_id AND s.session_key=r.lead_session_key
         WHERE r.run_id=?1 AND s.parent_session_key IS NULL",
    )?;
    let mut rows = statement.query([run_id.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let target = SessionTarget::new(parse(text(row, 0)?)?, run_id.clone(), parse(text(row, 1)?)?);
    let identity = NativeIdentity::new(
        provider(text(row, 2)?)?,
        path(row.get(3)?)?,
        parse(text(row, 4)?)?,
    );
    Ok(Some(LeadTarget::new(
        target,
        NativeBinding::new(identity, path(row.get(5)?)?),
    )))
}

pub(crate) fn identity_for_review(
    connection: &Connection,
    review: &ReviewId,
) -> Result<MrIdentity, StoreError> {
    let mut statement = connection
        .prepare_cached("SELECT origin, project_id, iid FROM reviews WHERE review_id=?1")?;
    let mut rows = statement.query([review.as_str()])?;
    identity(rows.next()?.ok_or(AppError::NotFound)?, 0)
}

pub(crate) fn find_review_id(
    connection: &Connection,
    source: &MrIdentity,
) -> Result<Option<ReviewId>, StoreError> {
    let value = connection
        .query_row(
            "SELECT review_id FROM reviews WHERE origin=?1 AND project_id=?2 AND iid=?3",
            params![
                source.origin().as_str(),
                source.project_id().as_sqlite_integer(),
                source.iid().as_sqlite_integer()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    value.map(|id| parse(&id)).transpose()
}

pub(crate) fn save_review(
    connection: &Connection,
    source: &ReviewSource,
    refresh: bool,
) -> Result<(ReviewId, bool), StoreError> {
    if source.project_path.is_empty()
        || source.project_path.len() > 4096
        || source.title.len() > 16 * 1024
    {
        return Err(AppError::InvalidRequest.into());
    }
    let id = find_review_id(connection, &source.identity)?;
    if let Some(review_id) = id {
        let changed = if refresh {
            connection.execute(
                "UPDATE reviews SET project_path=?2, title=?3, head_sha=?4, state=?5, updated_at_ms=?6
                 WHERE review_id=?1 AND updated_at_ms<=?6
                   AND (project_path, title, head_sha, state, updated_at_ms) IS DISTINCT FROM (?2, ?3, ?4, ?5, ?6)",
                params![review_id.as_str(), source.project_path, source.title, source.head_sha.as_str(),
                        state_label(source.state), source.updated_at_ms],
            )? == 1
        } else {
            false
        };
        return Ok((review_id, changed));
    }
    let review_id: ReviewId = parse(&Uuid::new_v4().to_string())?;
    connection.execute(
        "INSERT INTO reviews(review_id, origin, project_id, iid, project_path, title, head_sha, state, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![review_id.as_str(), source.identity.origin().as_str(), source.identity.project_id().as_sqlite_integer(),
                source.identity.iid().as_sqlite_integer(), source.project_path, source.title, source.head_sha.as_str(),
                state_label(source.state), source.updated_at_ms],
    )?;
    Ok((review_id, true))
}

fn queue_item(row: &Row<'_>) -> Result<QueueItem, StoreError> {
    Ok(QueueItem {
        review_id: parse(text(row, 0)?)?,
        source: ReviewSource {
            identity: identity(row, 1)?,
            project_path: row.get(4)?,
            title: row.get(5)?,
            head_sha: parse(text(row, 6)?)?,
            state: match text(row, 7)? {
                "open" => MergeRequestState::Open,
                "closed" => MergeRequestState::Closed,
                "merged" => MergeRequestState::Merged,
                _ => return Err(AppError::Internal.into()),
            },
            updated_at_ms: row.get(8)?,
        },
        registered_lead_count: u64::try_from(row.get::<_, i64>(9)?)
            .map_err(|_| AppError::Internal)?,
        decision: decisions::decode(row, 10)?,
        pending_source_updates: row.get(14)?,
        pending_native_attention: row.get(15)?,
        pending_report_attention: row.get(16)?,
    })
}

fn identity(row: &Row<'_>, offset: usize) -> Result<MrIdentity, StoreError> {
    let origin =
        GitLabOrigin::from_canonical(text(row, offset)?).map_err(|_| AppError::Internal)?;
    let project =
        ProjectId::try_from(row.get::<_, i64>(offset + 1)?).map_err(|_| AppError::Internal)?;
    let iid = MergeRequestIid::try_from(row.get::<_, i64>(offset + 2)?)
        .map_err(|_| AppError::Internal)?;
    Ok(MrIdentity::new(origin, project, iid))
}

fn state_label(state: MergeRequestState) -> &'static str {
    match state {
        MergeRequestState::Open => "open",
        MergeRequestState::Closed => "closed",
        MergeRequestState::Merged => "merged",
    }
}

pub(crate) fn provider(label: &str) -> Result<HostProvider, StoreError> {
    match label {
        "codex" => Ok(HostProvider::Codex),
        "claude" => Ok(HostProvider::Claude),
        _ => Err(AppError::Internal.into()),
    }
}

pub(crate) fn path(bytes: Vec<u8>) -> Result<AbsolutePath, StoreError> {
    AbsolutePath::from_canonical(OsString::from_vec(bytes).into())
        .map_err(|_| AppError::Internal.into())
}

pub(crate) fn parse<T: FromStr>(text: &str) -> Result<T, StoreError> {
    text.parse().map_err(|_| AppError::Internal.into())
}

pub(crate) fn text<'row>(row: &'row Row<'_>, col: usize) -> Result<&'row str, StoreError> {
    row.get_ref(col)?
        .as_str()
        .map_err(|_| AppError::Internal.into())
}
