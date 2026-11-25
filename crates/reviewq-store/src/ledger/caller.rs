//! Attributes a ledger call or a hook to a run's lead or to a verified child of it, inside the
//! transaction of the read or write it guards.

use reviewq_app::access::{Actor, AppError};
use reviewq_app::ledger::{CallBinding, Caller, CallingAgent, HookSession, HostSession};
use reviewq_domain::identity::{NativeSessionId, RunId, SessionKey};
use reviewq_domain::ledger::CallerRole;
use reviewq_domain::session::{HostProvider, LeadTarget};
use rusqlite::{Connection, params};

use crate::StoreError;
use crate::queue::{lead_for_run, parse};
use crate::reports::authority::{checkout_lead, run_lead};

/// The lead or verified child that made `binding`'s call. `Forbidden` when the call cannot be
/// attributed to a run's lead or a verified child of it; `Unauthorized` for a Codex request
/// without an active grant.
pub(super) fn resolve(db: &Connection, binding: &CallBinding) -> Result<Caller, StoreError> {
    let (lead, agent) = attribute(db, &binding.session)?;
    Ok(Caller {
        lead,
        agent,
        call_id: binding.call_id.clone(),
        turn: binding.turn.clone(),
    })
}

pub(super) fn attribute(
    db: &Connection,
    session: &HostSession,
) -> Result<(LeadTarget, CallingAgent), StoreError> {
    match session {
        HostSession::Claude { session, agent } => {
            let lead = known(run_lead(db, session))?.ok_or(AppError::Forbidden)?;
            if lead.binding().identity().provider() != HostProvider::Claude {
                return Err(AppError::Forbidden.into());
            }
            let agent = agent
                .clone()
                .map_or(CallingAgent::Lead, CallingAgent::ClaudeSubagent);
            Ok((lead, agent))
        }
        HostSession::Codex {
            thread,
            parent,
            actor,
        } => codex(db, thread, parent.as_ref(), actor),
    }
}

fn codex(
    db: &Connection,
    thread: &NativeSessionId,
    parent: Option<&NativeSessionId>,
    actor: &Actor,
) -> Result<(LeadTarget, CallingAgent), StoreError> {
    if let Some(lead) = granted(db, actor, thread.as_str())? {
        return Ok((lead, CallingAgent::Lead));
    }
    let mut lead = None;
    if let Some(parent) = parent {
        lead = granted(db, actor, parent.as_str())?;
        if lead.is_none() {
            lead = bound_child_lead(db, actor, parent)?;
        }
    }
    if lead.is_none() {
        lead = discovered_lead(db, actor, thread)?;
    }
    let lead = lead.ok_or(AppError::Forbidden)?;
    Ok((lead, CallingAgent::CodexThread(thread.clone())))
}

/// The lead whose registered Codex thread is `thread`, in a checkout the actor's grant
/// covers. A thread registered under two profiles cannot be told apart and is refused.
fn granted(db: &Connection, actor: &Actor, thread: &str) -> Result<Option<LeadTarget>, StoreError> {
    match checkout_lead(db, actor, HostProvider::Codex, thread) {
        Ok(lead) => Ok(Some(lead)),
        Err(error) => match error.app() {
            AppError::NotFound => Ok(None),
            AppError::Conflict => Err(AppError::Forbidden.into()),
            other => Err(other.into()),
        },
    }
}

/// The lead of the run `parent` is already bound to as a child: a discovered child session
/// of the run, or a thread whose earlier ledger call the run accepted.
fn bound_child_lead(
    db: &Connection,
    actor: &Actor,
    parent: &NativeSessionId,
) -> Result<Option<LeadTarget>, StoreError> {
    let runs = runs_of(
        db,
        "SELECT run_id FROM sessions WHERE native_id=?1 AND provider='codex'
           AND parent_session_key IS NOT NULL
         UNION SELECT run_id FROM ledger_calls WHERE provider='codex' AND native_session=?1
         LIMIT 2",
        parent,
    )?;
    lead_with_grant(db, actor, runs)
}

/// The lead of the run whose discovered children include `thread`.
fn discovered_lead(
    db: &Connection,
    actor: &Actor,
    thread: &NativeSessionId,
) -> Result<Option<LeadTarget>, StoreError> {
    let runs = runs_of(
        db,
        "SELECT run_id FROM sessions WHERE native_id=?1 AND provider='codex'
           AND parent_session_key IS NOT NULL LIMIT 2",
        thread,
    )?;
    lead_with_grant(db, actor, runs)
}

fn runs_of(
    db: &Connection,
    sql: &str,
    thread: &NativeSessionId,
) -> Result<Vec<String>, StoreError> {
    let mut statement = db.prepare_cached(sql)?;
    let rows = statement.query_map([thread.as_str()], |row| row.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The one run's lead, when the actor's grant covers its checkout.
fn lead_with_grant(
    db: &Connection,
    actor: &Actor,
    runs: Vec<String>,
) -> Result<Option<LeadTarget>, StoreError> {
    let [run] = runs.as_slice() else {
        return Ok(None);
    };
    let Some(lead) = lead_for_run(db, &parse(run)?)? else {
        return Ok(None);
    };
    let covered = granted(db, actor, lead.binding().identity().native_id().as_str())?;
    Ok(covered.filter(|granted| granted == &lead))
}

/// The run a lead-less session belonged to as its top-level session, when it is no longer
/// that run's registered lead. A closing call from it is refused with a failure.
pub(super) fn former_lead(
    db: &Connection,
    session: &HostSession,
) -> Result<Option<RunId>, StoreError> {
    let (sql, key) = match session {
        HostSession::Claude {
            session,
            agent: None,
        } => (
            "SELECT s.run_id FROM sessions s JOIN runs r USING(run_id)
             WHERE s.session_key=?1 AND s.parent_session_key IS NULL
               AND r.lead_session_key<>s.session_key",
            session.as_str(),
        ),
        HostSession::Codex { thread, .. } => (
            "SELECT s.run_id FROM sessions s JOIN runs r USING(run_id)
             WHERE s.native_id=?1 AND s.provider='codex' AND s.parent_session_key IS NULL
               AND r.lead_session_key<>s.session_key",
            thread.as_str(),
        ),
        HostSession::Claude { .. } => return Ok(None),
    };
    let mut statement = db.prepare_cached(sql)?;
    let runs: Vec<String> = statement
        .query_map([key], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    match runs.as_slice() {
        [run] => Ok(Some(parse(run)?)),
        _ => Ok(None),
    }
}

/// The session a hook names, its run, and whether it is that run's registered lead. `None`
/// for a session in no run, or a Codex thread that two profiles share.
pub(super) fn hook(
    db: &Connection,
    session: &HookSession,
) -> Result<Option<(RunId, SessionKey, bool)>, StoreError> {
    let (sql, key) = match session {
        HookSession::Claude(key) => (
            "SELECT s.run_id, s.session_key, r.lead_session_key=s.session_key
             FROM sessions s JOIN runs r USING(run_id) WHERE s.session_key=?1",
            key.as_str(),
        ),
        HookSession::Codex(thread) => (
            "SELECT s.run_id, s.session_key, r.lead_session_key=s.session_key
             FROM sessions s JOIN runs r USING(run_id)
             WHERE s.native_id=?1 AND s.provider='codex' LIMIT 2",
            thread.as_str(),
        ),
    };
    let mut statement = db.prepare_cached(sql)?;
    let rows: Vec<(String, String, bool)> = statement
        .query_map(params![key], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    match rows.as_slice() {
        [(run, session, lead)] => Ok(Some((parse(run)?, parse(session)?, *lead))),
        _ => Ok(None),
    }
}

/// The role of a call from `agent`: only the lead itself is the lead.
pub(super) fn role(agent: &CallingAgent) -> CallerRole {
    match agent {
        CallingAgent::Lead => CallerRole::Lead,
        CallingAgent::ClaudeSubagent(_) | CallingAgent::CodexThread(_) => CallerRole::Child,
    }
}

/// `Ok(None)` for `NotFound`, any other error unchanged.
fn known(result: Result<LeadTarget, StoreError>) -> Result<Option<LeadTarget>, StoreError> {
    match result {
        Ok(lead) => Ok(Some(lead)),
        Err(error) => match error.app() {
            AppError::NotFound => Ok(None),
            other => Err(other.into()),
        },
    }
}
