use std::collections::BTreeMap;

use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::history::{HistoryParent, HistorySession, HistoryStore};
use reviewq_domain::identity::{NativeInputId, NativeTurnId, RunId, SessionKey};
use reviewq_domain::session::{NativeBinding, NativeIdentity, SessionTarget};
use rusqlite::{Connection, params};

use crate::events::snapshot;
use crate::queue::{identity_for_review, parse, path, provider, text};
use crate::workspace::{allow_review, scope};
use crate::{SqliteStore, StoreError};

impl HistoryStore for SqliteStore {
    fn verified_history_turns(
        &self,
        actor: Actor,
        source: HistorySession,
        entries: Vec<NativeInputId>,
    ) -> AppFuture<'_, BTreeMap<NativeInputId, NativeTurnId>> {
        Box::pin(async move {
            if entries.len() > 100 {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |db| {
                let run=source.target.run_id().clone();
                let session=source.target.session_key().clone();
                if history_session(db,&actor,run.clone(),session.clone())?!=source {
                    return Err(AppError::CursorExpired.into());
                }
                let ids=serde_json::to_string(&entries.iter().map(|value|value.as_str()).collect::<Vec<_>>())
                    .map_err(|_|AppError::Internal)?;
                let mut result=BTreeMap::new();
                let mut select=db.prepare_cached(
                    "SELECT m.native_input_id,m.native_turn_id FROM json_each(?3) j JOIN messages m ON m.native_input_id=j.value
                     WHERE m.run_id=?1 AND m.session_key=?2 AND m.state='started' AND m.actor_key IS NOT NULL",
                )?;
                let mut rows=select.query(params![run.as_str(),session.as_str(),ids])?;
                while let Some(row)=rows.next()? {
                    let entry=parse(text(row,0)?)?;
                    let turn=parse(text(row,1)?)?;
                    if result.insert(entry,turn).is_some() {
                        return Err(AppError::Conflict.into());
                    }
                }
                Ok(result)
            }).await.map_err(StoreError::app)
        })
    }

    fn history_session(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
    ) -> AppFuture<'_, Snapshot<HistorySession>> {
        Box::pin(async move {
            self.read(move |connection| {
                let source = history_session(connection, &actor, run, session)?;
                snapshot(connection, source)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

pub(crate) fn history_session(
    connection: &Connection,
    actor: &Actor,
    run: RunId,
    session: SessionKey,
) -> Result<HistorySession, StoreError> {
    let access = scope(connection, actor)?;
    let mut query = connection.prepare_cached(
        "SELECT r.review_id,s.provider,s.profile,s.native_id,s.cwd,s.source_path,
                p.session_key,p.native_id,p.source_path,
                (SELECT root.native_id FROM sessions root WHERE root.session_key=r.lead_session_key AND root.run_id=r.run_id)
         FROM sessions s JOIN runs r ON r.run_id=s.run_id
         LEFT JOIN sessions p ON p.session_key=s.parent_session_key AND p.run_id=r.run_id
         WHERE s.run_id=?1 AND s.session_key=?2 AND
         (s.parent_session_key IS NULL OR p.provider=s.provider AND p.profile=s.profile)",
    )?;
    let mut rows = query.query(params![run.as_str(), session.as_str()])?;
    let row = rows.next()?.ok_or(AppError::NotFound)?;
    let review = parse(text(row, 0)?)?;
    let source = identity_for_review(connection, &review)?;
    allow_review(connection, access, &review, &source)?;
    let identity = NativeIdentity::new(
        provider(text(row, 1)?)?,
        path(row.get(2)?)?,
        parse(text(row, 3)?)?,
    );
    if matches!(actor.scope(), ActorScope::NativeSession(owner) if *owner != identity) {
        return Err(AppError::NotFound.into());
    }
    let parent = match row.get::<_, Option<String>>(6)? {
        None => None,
        Some(key) => Some(HistoryParent {
            session_key: parse(&key)?,
            native_id: parse(text(row, 7)?)?,
            root_native_id: if identity.provider() == reviewq_domain::session::HostProvider::Claude
            {
                row.get::<_, Option<String>>(9)?
                    .as_deref()
                    .map(parse)
                    .transpose()?
            } else {
                None
            },
            source: row.get::<_, Option<Vec<u8>>>(8)?.map(path).transpose()?,
        }),
    };
    Ok(HistorySession {
        target: SessionTarget::new(review, run, session),
        binding: NativeBinding::new(identity, path(row.get(4)?)?),
        source: row.get::<_, Option<Vec<u8>>>(5)?.map(path).transpose()?,
        parent,
    })
}
