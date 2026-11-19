use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::hosts::ProvenClaudeChild;
use reviewq_domain::session::LeadTarget;
use rusqlite::{OptionalExtension, params};

use super::workflow::ensure_lead;
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::queue::parse;
use crate::workspace::advance_queue;

pub(super) fn record(
    tx: &mut WriteTx<'_>,
    lead: LeadTarget,
    child: ProvenClaudeChild,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    ensure_lead(db, &lead)?;
    let root = lead.binding().identity();
    let native = child.binding.identity();
    if root.profile() != native.profile()
        || root.provider() != native.provider()
        || native.native_id() == root.native_id()
    {
        return Err(AppError::Forbidden.into());
    }
    let profile = root.profile().as_path().as_os_str().as_bytes();
    let run = lead.run_id().as_str();
    let parent = if let Some(parent) = child.parent {
        db.query_row(
            "SELECT session_key FROM sessions WHERE run_id=?1 AND provider='claude' AND profile=?2 AND native_id=?3",
            params![run,profile,parent.as_str()], |row| row.get::<_, String>(0),
        ).optional()?.ok_or(AppError::NotFound)?
    } else {
        lead.session_key().to_string()
    };
    let live: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM claude_companions WHERE session_key=?1 AND generation=?2 AND seen_at_ms>?3)",
        params![lead.session_key().as_str(), child.generation, now_ms()?.saturating_sub(30_000)],
        |row| row.get(0),
    )?;
    let sequence = i64::try_from(child.sequence).map_err(|_| AppError::InvalidRequest)?;
    if !live || sequence <= 0 {
        return Err(AppError::StaleRevision.into());
    }
    let (phase, _) = super::super::activity_columns(Some(child.activity));
    let id = native.native_id().as_str();
    let cwd = child.binding.cwd().as_path().as_os_str().as_bytes();
    let source = child.source.as_path().as_os_str().as_bytes();
    let key = format!("claude:{}:{id}", root.native_id());
    if key == parent {
        return Err(AppError::Conflict.into());
    }
    let changed = db.execute(
        "INSERT INTO sessions(session_key,run_id,provider,profile,native_id,cwd,source_path,host_activity,parent_session_key)
         VALUES(?1,?2,'claude',?3,?4,?5,?6,?7,?8)
         ON CONFLICT(provider,profile,native_id) DO UPDATE SET host_activity=excluded.host_activity
         WHERE sessions.run_id=excluded.run_id AND sessions.parent_session_key=excluded.parent_session_key
           AND sessions.cwd=excluded.cwd AND sessions.source_path=excluded.source_path
           AND (sessions.host_activity IS DISTINCT FROM excluded.host_activity OR NOT EXISTS (
                SELECT 1 FROM claude_children c WHERE c.session_key=sessions.session_key
                  AND c.companion_generation=?9
           ))
           AND NOT EXISTS (
                SELECT 1 FROM claude_children c WHERE c.session_key=sessions.session_key
                  AND c.companion_generation=?9 AND c.sequence>=?10
           )",
        params![key,run,profile,id,cwd,source,phase,parent,child.generation,sequence],
    )?;
    let bound = db.query_row(
        "SELECT session_key FROM sessions WHERE run_id=?1 AND provider='claude' AND profile=?2 AND native_id=?3
           AND cwd=?4 AND source_path=?5 AND parent_session_key=?6",
        params![run,profile,id,cwd,source,parent], |row| row.get::<_,String>(0),
    ).optional()?.ok_or(AppError::Conflict)?;
    let accepted = db.execute(
        "INSERT INTO claude_children(session_key,companion_generation,sequence) VALUES(?1,?2,?3)
         ON CONFLICT(session_key) DO UPDATE SET companion_generation=excluded.companion_generation,sequence=excluded.sequence
         WHERE companion_generation<>excluded.companion_generation OR sequence<excluded.sequence",
        params![bound, child.generation, sequence],
    )?;
    let metadata_changed =
        accepted != 0 && super::super::metadata::record(db, &bound, &child.metadata)?;
    if changed != 0 {
        advance_queue(db)?;
    }
    if changed != 0 || metadata_changed {
        let child = parse(&bound)?;
        tx.emit(&[
            ResourceKey::Queue,
            ResourceKey::Run(Some(lead.run_id().clone())),
            ResourceKey::Session(Some(child)),
        ])?;
    }
    Ok(())
}
