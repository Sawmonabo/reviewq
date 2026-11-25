//! Who may write a run's report: the configured project and grant, and the run's lead.

use reviewq_app::access::{Actor, ActorScope, AppError};
use reviewq_app::reports::ReportAuthority;
use reviewq_domain::identity::{MrIdentity, ReportHash, SessionKey};
use reviewq_domain::session::{HostProvider, LeadTarget};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::StoreError;
use crate::queue::{lead_for_run, parse};

/// Hashes the configured project, its GitLab principal and its checkout, or `None` when the
/// project is not configured. A changed hash means the report's authority changed.
#[expect(
    dead_code,
    reason = "K2 binds a ledger caller to the project authority"
)]
pub(crate) fn for_project(
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

/// The lead whose native session `native_id` is registered in a checkout the actor's active
/// grant covers. `Unauthorized` without a grant, `NotFound` or `Conflict` unless exactly one
/// lead matches.
#[expect(dead_code, reason = "K2 binds a Codex MCP caller to its lead")]
pub(crate) fn checkout_lead(
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
    run_lead(db, &parse(&key)?)
}

/// The lead of the run whose registered lead session is `session`, or `NotFound`.
pub(crate) fn run_lead(db: &Connection, session: &SessionKey) -> Result<LeadTarget, StoreError> {
    let run = db
        .query_row(
            "SELECT run_id FROM runs WHERE lead_session_key=?1",
            [session.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(AppError::NotFound)?;
    let lead = lead_for_run(db, &parse(&run)?)?.ok_or(AppError::NotFound)?;
    if lead.session_key() != session {
        return Err(AppError::NotFound.into());
    }
    Ok(lead)
}
