//! Persist native-discovered descendants under an already bound Codex parent.

use reviewq_app::history::HistorySession;
use reviewq_app::notifications::DiscoveredChildren;
use reviewq_domain::identity::{ReportHash, SessionKey};
use reviewq_domain::session::HostProvider;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::StoreError;
use crate::queue::parse;

use super::{activity_columns, bytes, metadata};

pub(crate) fn record(
    db: &Connection,
    parent: &HistorySession,
    page: DiscoveredChildren,
) -> Result<Vec<SessionKey>, StoreError> {
    let parent_key = parent.target.session_key().as_str();
    let run = parent.target.run_id().as_str();
    let owner = parent.binding.identity();
    let profile = bytes(owner.profile());
    let mut updates = Vec::new();
    for child in page.children {
        let native = child.binding.identity();
        if native.provider() != HostProvider::Codex
            || native.profile() != owner.profile()
            || native.native_id() == owner.native_id()
        {
            continue;
        }
        let id = native.native_id().as_str();
        let session_key = child_session_key(profile, id);
        let cwd = bytes(child.binding.cwd());
        let (phase, questions) = activity_columns(child.activity);
        let source = child.source.as_ref().map(bytes);
        let changed = db.execute(
            "INSERT INTO sessions(session_key,run_id,provider,profile,native_id,cwd,source_path,
               host_activity,host_questions,parent_session_key)
             VALUES(?1,?2,'codex',?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(provider,profile,native_id) DO UPDATE SET
               source_path=coalesce(excluded.source_path,sessions.source_path)
             WHERE sessions.run_id=excluded.run_id AND sessions.parent_session_key=excluded.parent_session_key
               AND sessions.cwd=excluded.cwd
               AND coalesce(excluded.source_path,sessions.source_path) IS DISTINCT FROM sessions.source_path
             ON CONFLICT(session_key) DO NOTHING",
            params![session_key, run, profile, id, cwd, source, phase, questions, parent_key],
        )? != 0;
        if !changed {
            continue;
        }
        let bound = db
            .query_row(
                "SELECT session_key FROM sessions WHERE run_id=?1 AND provider='codex'
               AND profile=?2 AND native_id=?3 AND cwd=?4 AND parent_session_key=?5",
                params![run, profile, id, cwd, parent_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(key) = bound {
            metadata::record(db, &key, &child.metadata, metadata::MetadataUpdate::Partial)?;
            updates.push(parse(&key)?);
        }
    }
    let next = page.next.as_ref().map(|next| next.cursor.as_str());
    let archived = page.next.as_ref().map(|next| next.archived);
    db.execute(
        "UPDATE sessions SET child_scan_cursor=?2,child_scan_archived=?3 WHERE session_key=?1
           AND (child_scan_cursor,child_scan_archived) IS DISTINCT FROM (?2,?3)",
        params![parent_key, next, archived],
    )?;
    Ok(updates)
}

fn child_session_key(profile: &[u8], native_id: &str) -> String {
    let mut identity = Sha256::new();
    identity.update((profile.len() as u64).to_be_bytes());
    identity.update(profile);
    identity.update(native_id);
    let digest = ReportHash::from_sha256(identity.finalize().into());
    format!("codex-child:{digest}")
}
