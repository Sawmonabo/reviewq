use reviewq_app::access::{Actor, ActorScope, AppError};
use reviewq_app::reports::ReportAuthority;
use reviewq_domain::identity::{MrIdentity, ReportHash};
use reviewq_domain::session::{HostProvider, LeadTarget};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::StoreError;

pub(super) fn for_project(
    db: &Connection,
    mr: &MrIdentity,
) -> Result<Option<ReportAuthority>, StoreError> {
    let material: Option<String> = db.query_row(
        "SELECT json_array(p.origin, p.project_id, s.origin, s.identity_id, hex(r.checkout), hex(r.common_dir))
         FROM configured_projects p
         LEFT JOIN settings s ON s.provider='gitlab'
         LEFT JOIN settings_repositories r USING(project_id)
         WHERE p.origin=?1 AND p.project_id=?2",
        params![mr.origin().as_str(), mr.project_id().as_sqlite_integer()],
        |row| row.get(0),
    ).optional()?;
    Ok(
        material
            .map(|value| ReportAuthority(ReportHash::from_sha256(Sha256::digest(value).into()))),
    )
}

pub(super) fn bind(
    db: &Connection,
    edits: Option<&str>,
    authority: &ReportAuthority,
    checkout: Option<&Actor>,
    arguments: Option<&ReportHash>,
    incomparable_recorded: i64,
) -> Result<String, StoreError> {
    let grant = match checkout.map(Actor::scope) {
        Some(ActorScope::CheckoutGrant(id)) => Some(id.as_str()),
        Some(_) => return Err(AppError::Forbidden.into()),
        None => None,
    };
    let epoch = checkout
        .and_then(Actor::grant_epoch)
        .map(|value| value.as_sqlite_integer());
    let metadata: String = db.query_row(
        r#"SELECT json_set(coalesce(?1, '{"findings":[],"sections":[]}'), '$.authority', ?2,
              '$.grantId', ?3, '$.grantEpoch', ?4, '$.argumentsSha256', ?5, '$.incomparableRecorded', ?6)"#,
        params![edits, authority.0.as_str(), grant, epoch, arguments.map(ReportHash::as_str), incomparable_recorded],
        |row| row.get(0),
    )?;
    if metadata.len() > 128 * 1024 {
        return Err(AppError::PayloadTooLarge.into());
    }
    Ok(metadata)
}

pub(super) fn checkout_lead(
    db: &Connection,
    actor: &Actor,
    provider: HostProvider,
    native_id: &str,
) -> Result<LeadTarget, StoreError> {
    let ActorScope::CheckoutGrant(grant) = actor.scope() else {
        return Err(AppError::Unauthorized.into());
    };
    let epoch = actor.grant_epoch().ok_or(AppError::Unauthorized)?;
    let mut statement = db.prepare_cached(
        "SELECT s.session_key FROM repo_grants g
         JOIN configured_projects p USING(origin,project_id)
         JOIN reviews r USING(origin,project_id)
         JOIN runs u USING(review_id)
         JOIN sessions s ON s.run_id=u.run_id AND s.session_key=u.lead_session_key
         WHERE g.grant_id=?1 AND g.epoch=?2 AND g.active=1 AND s.provider=?3 AND s.native_id=?4
         LIMIT 2",
    )?;
    let mut rows = statement.query(params![
        grant.as_str(),
        epoch.as_sqlite_integer(),
        crate::workspace::provider_label(provider),
        native_id
    ])?;
    let key: String = rows.next()?.ok_or(AppError::NotFound)?.get(0)?;
    if rows.next()?.is_some() {
        return Err(AppError::Conflict.into());
    }
    super::lead_for_session(db, &crate::queue::parse(&key)?)
}
