use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::ResourceKey;
use reviewq_app::history::HistorySession;
use reviewq_app::notifications::{
    Kind, NativeAttention, NativeAttentionStore, NativeChildCursor, NativeOccurrence, Observation,
    Source, TrackedNative,
};
use reviewq_domain::identity::{NotificationId, ReportHash, SessionKey};
use reviewq_domain::session::{HostProvider, SessionTarget};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use super::{insert_notification, publish};
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::history::history_session;
use crate::queue::parse;
use crate::workspace::advance_queue;
use crate::{SqliteStore, StoreError};

impl NativeAttentionStore for SqliteStore {
    fn tracked_native(&self, after: Option<SessionKey>) -> AppFuture<'_, Vec<TrackedNative>> {
        Box::pin(async move {
            self.read(move |db| tracked(db, after))
                .await
                .map_err(StoreError::app)
        })
    }

    fn record_native_attention(
        &self,
        target: HistorySession,
        observed: NativeAttention,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| record(tx, target, observed))
                .await
                .map_err(StoreError::app)
        })
    }
}

fn tracked(db: &Connection, after: Option<SessionKey>) -> Result<Vec<TrackedNative>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT s.run_id,s.session_key,f.native_turn_id,s.child_scan_cursor,s.child_scan_archived FROM sessions s JOIN runs x USING(run_id)
         JOIN reviews r USING(review_id) LEFT JOIN notification_native_failures f USING(session_key)
         WHERE s.provider='codex' AND r.state='open' AND s.session_key>?1
           AND EXISTS(SELECT 1 FROM configured_projects p WHERE p.origin=r.origin AND p.project_id=r.project_id)
         ORDER BY s.session_key LIMIT 8"
    )?;
    let rows = statement.query_map([after.as_ref().map_or("", |id| id.as_str())], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<bool>>(4)?,
        ))
    })?;
    let mut output = Vec::new();
    for row in rows {
        let (run, session, failure, cursor, archived) = row?;
        output.push(TrackedNative {
            session: history_session(db, &Actor::local_operator(), parse(&run)?, parse(&session)?)?,
            known_failure: failure.as_deref().map(parse).transpose()?,
            child_cursor: cursor.map(|cursor| NativeChildCursor {
                cursor,
                archived: archived.unwrap_or(false),
            }),
        });
    }
    Ok(output)
}

fn record(
    tx: &mut WriteTx<'_>,
    session: HistorySession,
    observed: NativeAttention,
) -> Result<(), StoreError> {
    let target = &session.target;
    let db = &tx.sql;
    let current = history_session(
        db,
        &Actor::local_operator(),
        target.run_id().clone(),
        target.session_key().clone(),
    )?;
    if session != current || session.binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::StaleRevision.into());
    }
    let configured: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM reviews r JOIN configured_projects p USING(origin,project_id)
         WHERE r.review_id=?1 AND r.state='open')",
        [target.review_id().as_str()],
        |row| row.get(0),
    )?;
    if !configured {
        return Err(AppError::StaleRevision.into());
    }
    let (phase, questions) = crate::hosts::activity_columns(observed.activity);
    let changed = db.execute(
        "UPDATE sessions SET host_activity=?2,host_questions=?3 WHERE session_key=?1
           AND (host_activity,host_questions) IS DISTINCT FROM (?2,?3)",
        params![target.session_key().as_str(), phase, questions],
    )? != 0;
    let metadata = crate::hosts::metadata::record(
        db,
        target.session_key().as_str(),
        &observed.metadata,
        crate::hosts::metadata::MetadataUpdate::Full,
    )?;
    let children = match observed.children {
        Some(page) => crate::hosts::children::record(db, &session, page)?,
        None => Vec::new(),
    };
    if let Some(failure) = observed.terminal_failure {
        db.execute(
            "INSERT INTO notification_native_failures VALUES(?1,?2)
             ON CONFLICT(session_key) DO UPDATE SET native_turn_id=excluded.native_turn_id
             WHERE native_turn_id<>excluded.native_turn_id",
            params![target.session_key().as_str(), failure.as_str()],
        )?;
    }
    let started: i64 = db.query_row(
        "SELECT started_at_ms FROM runs WHERE run_id=?1",
        [target.run_id().as_str()],
        |row| row.get(0),
    )?;
    let mut ids = Vec::new();
    for event in observed.events {
        if matches!(event.kind, Kind::AgentFailure | Kind::Quota)
            && event.at_ms.is_none_or(|time| time < started)
        {
            continue;
        }
        if let Some(id) = insert(db, HostProvider::Codex, target, event)? {
            ids.push(id);
        }
    }
    if (changed || !children.is_empty()) && ids.is_empty() {
        advance_queue(&tx.sql)?;
    }
    if changed || metadata || !children.is_empty() {
        let mut keys = vec![
            ResourceKey::Queue,
            ResourceKey::Review(Some(target.review_id().clone())),
            ResourceKey::Run(Some(target.run_id().clone())),
            ResourceKey::Session(Some(target.session_key().clone())),
        ];
        keys.extend(
            children
                .into_iter()
                .map(|child| ResourceKey::Session(Some(child))),
        );
        tx.emit(&keys)?;
    }
    if let Some(failure) = observed.recoverable {
        crate::hosts::recovery::observe_failure(tx, &session, failure)?;
    }
    publish(tx, &ids)
}

pub(crate) fn insert(
    db: &Connection,
    provider: HostProvider,
    target: &SessionTarget,
    event: NativeOccurrence,
) -> Result<Option<NotificationId>, StoreError> {
    if event.native_ref.is_empty()
        || event.native_ref.len() > 512
        || event.native_ref.chars().any(char::is_control)
    {
        return Err(AppError::InvalidRequest.into());
    }
    let label = match event.kind {
        Kind::AgentQuestion => "An agent has a question",
        Kind::AgentPermission => "An agent is waiting on permission",
        Kind::AgentFailure => "An agent turn failed",
        Kind::Quota => "Codex usage is limited",
        _ => return Err(AppError::InvalidRequest.into()),
    };
    let source = match provider {
        HostProvider::Claude => Source::Claude,
        HostProvider::Codex => Source::Codex,
    };
    let mut identity = Sha256::new();
    let kind = event.kind.to_string();
    let (scope, occurrence) = match (event.kind, event.desktop_episode.as_deref()) {
        (Kind::Quota, Some(episode)) => (target.review_id().as_str(), episode),
        _ => (target.session_key().as_str(), event.native_ref.as_str()),
    };
    for part in [scope, kind.as_str(), occurrence] {
        crate::workspace::hash_field(&mut identity, part.as_bytes());
    }
    insert_notification(
        db,
        Observation {
            source,
            occurrence_id: ReportHash::from_sha256(identity.finalize().into()).to_string(),
            kind: event.kind,
            review: target.review_id().clone(),
            run: Some(target.run_id().clone()),
            session: Some(target.session_key().clone()),
            event_ref: Some(event.native_ref),
            label: label.to_owned(),
            occurred_at_ms: event.at_ms.unwrap_or(now_ms()?),
            desktop_episode: event.desktop_episode,
        },
    )
}
