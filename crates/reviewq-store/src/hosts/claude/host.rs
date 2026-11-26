//! The process hosting each companion generation, as its heartbeats name it, and when a
//! stored companion's host is proven dead. `ClaudeHost::is_dead` is the one test of death;
//! this module only decides which stored hosts it may be asked about.

use reviewq_app::hosts::ClaudeHost;
use rusqlite::{Connection, OptionalExtension, Row};

use crate::StoreError;

/// How far apart two reports of one generation must be before the host they both name is
/// trusted. A helper started through a process of its own for each call would name a new
/// parent each time, so one that names the same process twice, this far apart, names the
/// process that runs the session.
const TRUST_AFTER_MS: i64 = 5_000;

/// The report names the stored generation and the same host.
const SAME: &str = "(generation=excluded.generation AND excluded.host_pid IS NOT NULL
    AND host_pid IS excluded.host_pid AND host_start IS excluded.host_start
    AND host_boot IS excluded.host_boot AND host_namespace IS excluded.host_namespace)";

/// The heartbeat upsert's host assignments. `excluded` is the report, which names its host as
/// `?12` to `?15` and its time as `seen_at_ms`; every other name is the stored row as it was.
pub(super) fn upsert() -> String {
    format!(
        "host_trusted=CASE WHEN {SAME}
           THEN max(host_trusted, excluded.seen_at_ms-host_since_ms>={TRUST_AFTER_MS}) ELSE 0 END,
         host_since_ms=CASE WHEN {SAME} THEN host_since_ms ELSE excluded.host_since_ms END,
         host_pid=excluded.host_pid, host_start=excluded.host_start,
         host_boot=excluded.host_boot, host_namespace=excluded.host_namespace"
    )
}

/// The stored companion's host columns, in the order `trusted` reads them.
pub(super) const COLUMNS: &str =
    "c.host_trusted, c.host_pid, c.host_start, c.host_boot, c.host_namespace";

/// The trusted host in the five `COLUMNS` starting at `first`, or `None`.
pub(super) fn trusted(row: &Row<'_>, first: usize) -> rusqlite::Result<Option<ClaudeHost>> {
    if !row.get::<_, bool>(first)? {
        return Ok(None);
    }
    let (pid, start): (i64, i64) = (row.get(first + 1)?, row.get(first + 2)?);
    let (Ok(pid), Ok(start_time)) = (u32::try_from(pid), u64::try_from(start)) else {
        return Ok(None);
    };
    Ok(Some(ClaudeHost {
        pid,
        start_time,
        boot_id: row.get(first + 3)?,
        pid_namespace: row.get(first + 4)?,
    }))
}

/// The generation of `session`'s companion when it is another than `reporting`, not gone, and
/// its trusted host is proven dead, as another generation's report checks before it takes the
/// session over.
pub(super) fn dead_generation(
    db: &Connection,
    session: &str,
    reporting: &str,
) -> Result<Option<String>, StoreError> {
    let found = db
        .prepare_cached(&format!(
            "SELECT c.generation, {COLUMNS} FROM claude_companions c
             WHERE c.session_key=?1 AND c.generation<>?2 AND c.host_trusted=1
               AND NOT reviewq_companion_gone(c.seen_at_ms)"
        ))?
        .query_row([session, reporting], |row| {
            Ok((row.get::<_, String>(0)?, trusted(row, 1)?))
        })
        .optional()?;
    Ok(found.and_then(|(generation, host)| {
        host.is_some_and(|host| host.is_dead())
            .then_some(generation)
    }))
}

/// The report's host as the upsert's `?12` to `?15`, or all `NULL` without one.
pub(super) type HostParams<'a> = (Option<i64>, Option<i64>, Option<&'a str>, Option<&'a str>);

pub(super) fn params(host: Option<&ClaudeHost>) -> HostParams<'_> {
    let Some((host, start)) =
        host.and_then(|host| Some((host, i64::try_from(host.start_time).ok()?)))
    else {
        return (None, None, None, None);
    };
    (
        Some(i64::from(host.pid)),
        Some(start),
        Some(host.boot_id.as_str()),
        Some(host.pid_namespace.as_str()),
    )
}
