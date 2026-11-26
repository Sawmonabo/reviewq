//! Bounded review observations, scoped projections, and exact review selection.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::str::FromStr;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::queue::{
    CompactTicket, MAX_QUEUE_PAGE, MergeRequestState, PageContext, QueueFilter, QueueItem,
    QueuePage, QueuePosition, QueueStore, ReviewAuthor, ReviewOverview, ReviewSignals,
    ReviewSource, RunDetail, RunPosition, RunSummary, SessionPosition, SessionSummary,
};
use reviewq_domain::identity::{
    EventCursor, GitLabOrigin, MergeRequestIid, MrIdentity, ProjectId, ReviewId, RunId,
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

mod archive;
mod cached;
pub(crate) mod discussions;
mod filters;
mod focus;
mod lookup;
mod management;
mod sprints;
pub(crate) mod stage;
pub(crate) mod tracking;
mod workspace;

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
             )),
     (SELECT json_group_array(json_array(ticket_key,title)) FROM (
         SELECT ticket_key, coalesce(min(CASE WHEN retained THEN title END), min(title)) AS title
         FROM (
             SELECT i.ticket_key, i.title, 0 AS retained FROM scoped_review_issues si
             JOIN scoped_issues i USING(board_id,ticket_key) WHERE si.review_id=r.review_id
             UNION ALL SELECT ticket_key, title, 1 FROM retained_review_tickets WHERE review_id=r.review_id
         ) GROUP BY ticket_key ORDER BY ticket_key)), r.author_id, r.author_username, r.draft,
     f.own, f.personal_approval, f.our_comments, f.unread_replies, f.has_questions,
     f.stage_reason, f.queue_state, f.focus_run, f.focus_session, f.full_mr_reviewed_sha, f.coverage, f.last_activity";
const ITEM_FROM: &str = "FROM reviews AS r LEFT JOIN review_decisions AS d USING(review_id)
                         JOIN review_activity AS f ON f.review_id=r.review_id";
const CACHED_ITEM_FROM: &str = "FROM reviews AS r LEFT JOIN review_decisions AS d USING(review_id)
                         JOIN queue_projection AS f ON f.review_id=r.review_id";
const SCOPE_PREDICATE: &str =
    "(((?1 = 0 OR ?1 = 1)
       AND EXISTS (SELECT 1 FROM configured_projects c WHERE c.origin=r.origin AND c.project_id=r.project_id)
       AND (EXISTS (SELECT 1 FROM review_discoveries d WHERE d.review_id=r.review_id)
            OR EXISTS (SELECT 1 FROM retained_reviews WHERE review_id=r.review_id)))
      OR (?1 = 0 AND EXISTS (SELECT 1 FROM retained_reviews WHERE review_id=r.review_id))
      OR (?1 = 2 AND r.review_id=?2))";

impl QueueStore for SqliteStore {
    fn queue_readiness(
        &self,
        actor: Actor,
    ) -> AppFuture<'_, Snapshot<reviewq_app::queue::QueueReadiness>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.read(|db| {
                let settings = settings_revision(db)?;
                let (projects,total) = db.query_row(
                    "SELECT (SELECT count(*) FROM configured_projects),
                      (SELECT count(*) FROM reviews r JOIN configured_projects p USING(origin,project_id)
                       WHERE r.state='open' AND (EXISTS(SELECT 1 FROM review_discoveries d WHERE d.review_id=r.review_id)
                            OR EXISTS(SELECT 1 FROM review_tracking t WHERE t.review_id=r.review_id)))",
                    [],
                    |row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?))
                )?;
                snapshot(db,reviewq_app::queue::QueueReadiness {
                    settings,projects:projects.try_into().map_err(|_|AppError::Internal)?,
                    total_mrs:total.try_into().map_err(|_|AppError::Internal)?,
                    sync:crate::refresh::read_sync(db,settings)?,
                })
            }).await.map_err(StoreError::app)
        })
    }
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
        filter: QueueFilter,
        after: Option<QueuePosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<QueuePage>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |tx| snapshot(tx, filters::queue(tx, actor, filter, after, limit)?))
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
    let mut inbox = Vec::new();
    for item in source {
        let (id, written) = save_review(&tx.sql, item, true)?;
        if written && item.state != MergeRequestState::Open {
            // The review is over: its passes end and its report failures no longer need the
            // reviewer.
            crate::ledger::passes::end_for_review(&tx.sql, &id, crate::events::now_ms()?)?;
            let alerts = crate::reports::failures::unresolved_alerts(&tx.sql, &id, None)?;
            inbox.extend(crate::notifications::withdraw(&tx.sql, alerts)?);
        }
        if written {
            if let Some(id) = crate::notifications::gitlab::observe_head(
                &tx.sql,
                &id,
                &item.head_sha,
                item.updated_at_ms,
            )? {
                inbox.push(id);
            }
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
    crate::notifications::publish(tx, &inbox)?;
    Ok(results)
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
    make_context(connection, actor, limit, previous, None)
}

fn make_context(
    connection: &Connection,
    actor: &Actor,
    limit: u16,
    previous: Option<&PageContext>,
    projection: Option<i64>,
) -> Result<PageContext, StoreError> {
    let context = PageContext {
        generation: queue_revision(connection)?,
        authority: actor_key(actor),
        limit,
        projection,
    };
    if previous.is_some_and(|previous| previous != &context) {
        return Err(AppError::CursorExpired.into());
    }
    Ok(context)
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
        "SELECT {ITEM_COLUMNS} {ITEM_FROM} WHERE r.review_id=?1"
    ))?;
    let mut rows = statement.query([review_id.as_str()])?;
    let mut item = queue_item(rows.next()?.ok_or(AppError::NotFound)?)?;
    drop(rows);
    focus::hydrate(connection, std::iter::once(&mut item))?;

    let (time, id) = after.as_ref().map_or((i64::MAX, "~"), |position| {
        (position.started_at_ms, position.run_id.as_str())
    });
    let mut statement = connection.prepare_cached(
        "SELECT r.run_id, r.started_at_ms, r.lead_session_key, s.provider, s.native_name
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
            lead_name: row.get(4)?,
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
    let settings_revision = settings_revision(connection)?;
    let our_discussions = connection
        .query_row(
            "SELECT our_threads, open_threads FROM review_source_signals
         WHERE review_id=?1 AND settings_revision=?2 AND source_sha=?3 AND our_threads IS NOT NULL",
            params![
                review_id.as_str(),
                settings_revision.as_sqlite_integer(),
                item.source.head_sha.as_str()
            ],
            |row| {
                Ok(reviewq_app::queue::DiscussionCounts {
                    total: row.get(0)?,
                    open: row.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(ReviewOverview {
        item,
        our_discussions,
        settings_revision,
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
    let lead_metadata = crate::hosts::metadata::read(connection, lead.session_key().as_str())?;
    let mut statement = connection.prepare_cached(
        "SELECT s.session_key, s.parent_session_key, s.provider, s.native_id, s.cwd,
                s.native_name, coalesce(s.native_status, s.host_activity), s.native_started_at_ms,
                coalesce(s.native_model, c.model), coalesce(s.native_effort, c.effort)
         FROM sessions s LEFT JOIN claude_companions c USING(session_key)
         WHERE s.run_id=?1 AND s.session_key>?2 ORDER BY s.session_key LIMIT ?3",
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
            native_id: parse(text(row, 3)?)?,
            cwd: path(row.get(4)?)?,
            metadata: crate::hosts::metadata::from_row(row, 5)?,
        });
    }
    let next_session = if sessions.len() > usize::from(limit) {
        sessions.pop();
        sessions.last().map(|session| SessionPosition {
            context,
            run_id: run_id.clone(),
            session_key: session.session_key.clone(),
        })
    } else {
        None
    };
    Ok(RunDetail {
        lead,
        lead_metadata,
        sessions,
        next_session,
        launch: crate::launches::run_launch_details(connection, &run_id)?,
        request_evidence: crate::launches::request_evidence(connection, &run_id)?,
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

pub(crate) fn source_for_review(
    db: &Connection,
    review: &ReviewId,
) -> Result<ReviewSource, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT origin,project_id,iid,project_path,title,head_sha,state,updated_at_ms,author_id,author_username,draft
         FROM reviews WHERE review_id=?1")?;
    let mut rows = statement.query([review.as_str()])?;
    let row = rows.next()?.ok_or(AppError::NotFound)?;
    Ok(ReviewSource {
        identity: identity(row, 0)?,
        project_path: row.get(3)?,
        title: row.get(4)?,
        head_sha: parse(text(row, 5)?)?,
        state: parse_state(text(row, 6)?)?,
        updated_at_ms: row.get(7)?,
        author: read_author(row, 8)?,
        draft: row.get(10)?,
    })
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
    let author_id = author_number(source)?;
    let author_name = source
        .author
        .as_ref()
        .map(|author| author.username.as_str());
    if let Some(review_id) = id {
        let changed = refresh
            && connection.execute(
                "UPDATE reviews SET project_path=?2, title=?3, head_sha=?4, state=?5, updated_at_ms=?6,
                 author_id=?7,author_username=?8,draft=?9,observation_cursor=(SELECT event_head+1 FROM service_state WHERE singleton=1)
                 WHERE review_id=?1 AND updated_at_ms<=?6
                   AND (project_path,title,head_sha,state,updated_at_ms,author_id,author_username,draft)
                        IS DISTINCT FROM (?2,?3,?4,?5,?6,?7,?8,?9)",
                params![review_id.as_str(), source.project_path, source.title, source.head_sha.as_str(),
                        state_label(source.state), source.updated_at_ms,author_id,author_name,source.draft],
            )? == 1;
        return Ok((review_id, changed));
    }
    let review_id: ReviewId = parse(&Uuid::new_v4().to_string())?;
    connection.execute(
        "INSERT INTO reviews(review_id, origin, project_id, iid, project_path, title, head_sha, state,
          updated_at_ms, author_id,author_username,draft,observation_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,?10,?11,?12,
           (SELECT event_head+1 FROM service_state WHERE singleton=1))",
        params![review_id.as_str(), source.identity.origin().as_str(), source.identity.project_id().as_sqlite_integer(),
                source.identity.iid().as_sqlite_integer(), source.project_path, source.title, source.head_sha.as_str(),
                state_label(source.state), source.updated_at_ms,author_id,author_name,source.draft],
    )?;
    Ok((review_id, true))
}

pub(crate) fn check_observation(
    db: &Connection,
    before: EventCursor,
    item: &ReviewSource,
) -> Result<(), StoreError> {
    let newer = db
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM reviews r WHERE origin=?1 AND project_id=?2 AND iid=?3
          AND observation_cursor>?4
          AND (project_path,title,head_sha,state,updated_at_ms,author_id,author_username,draft)
            IS DISTINCT FROM (?5,?6,?7,?8,?9,?10,?11,?12))",
        )?
        .query_row(
            params![
                item.identity.origin().as_str(),
                item.identity.project_id().as_sqlite_integer(),
                item.identity.iid().as_sqlite_integer(),
                before.as_sqlite_integer(),
                item.project_path,
                item.title,
                item.head_sha.as_str(),
                state_label(item.state),
                item.updated_at_ms,
                author_number(item)?,
                item.author.as_ref().map(|author| author.username.as_str()),
                item.draft,
            ],
            |row| row.get::<_, bool>(0),
        )?;
    if newer {
        return Err(AppError::StaleRevision.into());
    }
    Ok(())
}

fn queue_item(row: &Row<'_>) -> Result<QueueItem, StoreError> {
    let tickets = serde_json::from_str::<Vec<(String, String)>>(text(row, 17)?)
        .map_err(|_| AppError::Internal)?
        .into_iter()
        .map(|(key, title)| {
            Ok(CompactTicket {
                key: parse(&key)?,
                title,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(QueueItem {
        review_id: parse(text(row, 0)?)?,
        source: ReviewSource {
            identity: identity(row, 1)?,
            project_path: row.get(4)?,
            title: row.get(5)?,
            head_sha: parse(text(row, 6)?)?,
            state: parse_state(text(row, 7)?)?,
            updated_at_ms: row.get(8)?,
            author: read_author(row, 18)?,
            draft: row.get(20)?,
        },
        tickets,
        registered_lead_count: u64::try_from(row.get::<_, i64>(9)?)
            .map_err(|_| AppError::Internal)?,
        decision: decisions::decode(row, 10)?,
        pending_source_updates: row.get(14)?,
        pending_native_attention: row.get(15)?,
        pending_report_attention: row.get(16)?,
        signals: ReviewSignals {
            own: row.get(21)?,
            personal_approval: row.get(22)?,
            our_comments: u64::try_from(row.get::<_, i64>(23)?).map_err(|_| AppError::Internal)?,
            unread_replies: u64::try_from(row.get::<_, i64>(24)?)
                .map_err(|_| AppError::Internal)?,
            has_questions: row.get(25)?,
            stage_reason: row.get(26)?,
            queue_state: row.get(27)?,
            run: row
                .get::<_, Option<String>>(28)?
                .as_deref()
                .map(parse)
                .transpose()?,
            focus: row
                .get::<_, Option<String>>(29)?
                .as_deref()
                .map(parse)
                .transpose()?,
            focused_provider: None,
            focused_lead_name: None,
            focused_activity: None,
            registered_providers: Vec::new(),
            unread_non_reply_count: 0,
            focused_launch: None,
            focused_report_reviewed_sha: None,
            full_mr_reviewed_sha: row
                .get::<_, Option<String>>(30)?
                .as_deref()
                .map(parse)
                .transpose()?,
            coverage: row.get(31)?,
            last_activity_ms: row.get(32)?,
        },
    })
}

fn read_author(row: &Row<'_>, offset: usize) -> Result<Option<ReviewAuthor>, StoreError> {
    let id: Option<i64> = row.get(offset)?;
    id.map(|id| {
        Ok(ReviewAuthor {
            id: u64::try_from(id).map_err(|_| AppError::Internal)?,
            username: row.get(offset + 1)?,
        })
    })
    .transpose()
}

pub(crate) fn author_number(review: &ReviewSource) -> Result<Option<i64>, StoreError> {
    Ok(review
        .author
        .as_ref()
        .map(|author| i64::try_from(author.id))
        .transpose()
        .map_err(|_| AppError::InvalidRequest)?)
}

fn parse_state(value: &str) -> Result<MergeRequestState, StoreError> {
    match value {
        "open" => Ok(MergeRequestState::Open),
        "closed" => Ok(MergeRequestState::Closed),
        "merged" => Ok(MergeRequestState::Merged),
        _ => Err(AppError::Internal.into()),
    }
}

pub(crate) fn identity(row: &Row<'_>, offset: usize) -> Result<MrIdentity, StoreError> {
    let origin =
        GitLabOrigin::from_canonical(text(row, offset)?).map_err(|_| AppError::Internal)?;
    let project =
        ProjectId::try_from(row.get::<_, i64>(offset + 1)?).map_err(|_| AppError::Internal)?;
    let iid = MergeRequestIid::try_from(row.get::<_, i64>(offset + 2)?)
        .map_err(|_| AppError::Internal)?;
    Ok(MrIdentity::new(origin, project, iid))
}

pub(crate) fn state_label(state: MergeRequestState) -> &'static str {
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
