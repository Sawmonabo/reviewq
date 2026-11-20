//! Optional display metadata lives beside the authoritative database.

mod layout;
mod window;

pub(super) use window::Builds;

use std::path::PathBuf;
use std::time::Duration;

use reviewq_app::access::AppError;
use reviewq_app::reports::{DisplayFallback, DisplayIndex, ReportReference};
use reviewq_domain::identity::RunId;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::profile::Profile;
use super::staging::{self, Stage};
use crate::{SqliteStore, StoreError};

const SUFFIX: &str = ".findings-display-v2";
const APPLICATION_ID: i64 = 0x5256_4644;
const SCHEMA_VERSION: i64 = 2;
const PAGE_BYTES: i64 = 4096;
// Keep the database at or below 16 MiB. A pending DELETE journal is also bounded by that page count.
const MAX_PAGES: i64 = 4096;
const CACHE_ENTRIES: i64 = 32;
const MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const WRITE_FLAGS: OpenFlags =
    OpenFlags::SQLITE_OPEN_READ_WRITE.union(OpenFlags::SQLITE_OPEN_NOFOLLOW);

const SCHEMA: &str = "
CREATE TABLE cache_owner (
    singleton INTEGER PRIMARY KEY CHECK (singleton=1),
    store_id TEXT NOT NULL
) STRICT;

CREATE TABLE display_entries (
    id INTEGER PRIMARY KEY,
    artifact_id TEXT NOT NULL,
    sha256 TEXT NOT NULL CHECK (length(sha256)=64),
    index_format INTEGER NOT NULL CHECK (index_format>0),
    published_at_ms INTEGER NOT NULL,
    span_count INTEGER NOT NULL CHECK (span_count BETWEEN 0 AND 512),
    byte_length INTEGER NOT NULL CHECK (byte_length BETWEEN 0 AND 1048576),
    scalar_length INTEGER NOT NULL CHECK (scalar_length BETWEEN 0 AND byte_length),
    utf16_length INTEGER NOT NULL CHECK (utf16_length BETWEEN scalar_length AND byte_length),
    fallback TEXT,
    checksum BLOB NOT NULL CHECK (length(checksum)=32),
    CHECK ((fallback IS NULL) = (span_count>0)),
    UNIQUE (artifact_id, sha256, index_format)
) STRICT;

CREATE TABLE display_spans (
    entry_id INTEGER NOT NULL REFERENCES display_entries(id) ON DELETE CASCADE,
    byte_start INTEGER NOT NULL CHECK (byte_start>=0),
    byte_end INTEGER NOT NULL CHECK (byte_end>byte_start),
    scalar_start INTEGER NOT NULL CHECK (scalar_start BETWEEN 0 AND byte_start),
    utf16_start INTEGER NOT NULL CHECK (utf16_start BETWEEN scalar_start AND byte_start),
    kind TEXT NOT NULL,
    heading_level INTEGER CHECK (heading_level BETWEEN 1 AND 6),
    heading_markdown TEXT CHECK (length(CAST(heading_markdown AS BLOB))<=1024),
    checksum BLOB NOT NULL CHECK (length(checksum)=32),
    CHECK ((kind='heading')=(heading_level IS NOT NULL)),
    PRIMARY KEY (entry_id, byte_start)
) STRICT, WITHOUT ROWID;
";

impl SqliteStore {
    pub(crate) async fn cache_report_display(
        &self,
        run: RunId,
        reference: ReportReference,
        source: Option<String>,
    ) -> Result<(), StoreError> {
        let hash = reference.hash;
        let confirmed_hash = hash.clone();
        // Resolve the sealed artifact on the normal reader and release its queue slot
        // before opening or writing the independent cache.
        let (artifact_id, profile) = self
            .submit(false, move |db, profile| {
                let artifact_id: String = db
                    .query_row(
                        "SELECT a.artifact_id FROM report_versions v JOIN artifacts a USING(artifact_id)
                         WHERE v.run_id=?1 AND v.receipt_id=?2 AND a.sha256=?3 AND a.sealed=1 AND a.complete=1",
                        params![run.as_str(), reference.receipt_id.as_str(), confirmed_hash.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or(AppError::NotFound)?;
                Ok((artifact_id, profile.clone()))
            })
            .await?;
        self.build_captured_index(profile, artifact_id, hash, source)
            .await
    }
}

fn open(profile: &Profile, store_id: Uuid, create: bool) -> Result<Option<Connection>, StoreError> {
    let mut path = profile.path().as_os_str().to_os_string();
    path.push(SUFFIX);
    let path = PathBuf::from(path);
    let exists = profile.file(SUFFIX)?.is_some();
    let journal = profile.file(&format!("{SUFFIX}-journal"))?;
    if (!exists && journal.is_some())
        || profile.file(&format!("{SUFFIX}-wal"))?.is_some()
        || profile.file(&format!("{SUFFIX}-shm"))?.is_some()
    {
        return Err(AppError::Conflict.into());
    }
    if !exists && !create {
        return Ok(None);
    }
    let connection = if exists {
        Connection::open_with_flags(&path, WRITE_FLAGS)?
    } else {
        let stage = Stage::new(profile)?;
        let file = staging::create_private(&stage.database)?;
        let mut connection = Connection::open_with_flags(&stage.database, WRITE_FLAGS)?;
        connection.pragma_update(None, "page_size", PAGE_BYTES)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.execute(
            "INSERT INTO cache_owner(singleton, store_id) VALUES(1, ?1)",
            [store_id.to_string()],
        )?;
        tx.commit()?;
        drop(connection);
        file.sync_all()?;
        if let Err(error) = profile.publish_sibling(&stage.database, SUFFIX)
            && profile.file(SUFFIX)?.is_none()
        {
            return Err(error);
        }
        // The other build lane can win the first publication. Verify its cache below.
        Connection::open_with_flags(&path, WRITE_FLAGS)?
    };
    configure(&connection, store_id)?;
    Ok(Some(connection))
}

fn configure(connection: &Connection, store_id: Uuid) -> Result<(), StoreError> {
    connection.busy_timeout(Duration::from_millis(150))?;
    connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON;")?;
    if connection.pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))?
        != APPLICATION_ID
        || connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?
            != SCHEMA_VERSION
        || connection.pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))?
            != PAGE_BYTES
        || connection.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?
            != "delete"
        || connection.query_row(
            "SELECT store_id FROM cache_owner WHERE singleton=1",
            [],
            |row| row.get::<_, String>(0),
        )? != store_id.to_string()
    {
        return Err(AppError::Conflict.into());
    }
    connection.pragma_update(None, "max_page_count", MAX_PAGES)?;
    let page_limit =
        connection.pragma_query_value(None, "max_page_count", |row| row.get::<_, i64>(0))?;
    if page_limit != MAX_PAGES {
        return Err(AppError::Conflict.into());
    }
    Ok(())
}

fn publish(
    connection: &mut Connection,
    artifact_id: &str,
    hash: &str,
    index_format: u32,
    index: DisplayIndex,
) -> Result<(), StoreError> {
    let time = crate::events::now_ms()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "DELETE FROM display_entries WHERE published_at_ms < ?1",
        [time.saturating_sub(MAX_AGE_MS)],
    )?;
    let (layout, span_count, fallback) = match index {
        DisplayIndex::Indexed(layout) => {
            let span_count = layout.blocks.len() as u32;
            (Some(layout), span_count, None)
        }
        DisplayIndex::Fallback(reason) => (None, 0, Some(fallback_label(reason))),
    };
    let (bytes, scalars, utf16) = layout.as_ref().map_or((0, 0, 0), |layout| {
        (
            layout.byte_length,
            layout.scalar_length,
            layout.utf16_length,
        )
    });
    let checksum = layout::header_checksum(hash, [span_count, bytes, scalars, utf16], fallback);
    // Make room before writing spans, while retaining the current entry on
    // an idempotent publish. A write failure rolls back the eviction as well.
    tx.execute(
        "DELETE FROM display_entries WHERE id IN (
             SELECT id FROM display_entries ORDER BY id DESC LIMIT -1 OFFSET ?1
         ) AND NOT EXISTS (
             SELECT 1 FROM display_entries WHERE artifact_id=?2 AND sha256=?3 AND index_format=?4
         )",
        params![CACHE_ENTRIES - 1, artifact_id, hash, index_format],
    )?;
    let inserted = tx.execute(
        "INSERT INTO display_entries(artifact_id,sha256,index_format,published_at_ms,span_count,byte_length,scalar_length,utf16_length,fallback,checksum)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(artifact_id,sha256,index_format) DO NOTHING",
        params![artifact_id, hash, index_format, time, span_count, bytes, scalars, utf16, fallback, checksum.as_slice()],
    )?;
    if inserted == 1
        && let Some(layout) = layout
    {
        layout::write_spans(&tx, tx.last_insert_rowid(), hash, layout.blocks)?;
    }
    tx.commit()?;
    Ok(())
}

fn fallback_label(reason: DisplayFallback) -> &'static str {
    match reason {
        DisplayFallback::DocumentBudget => "document_budget",
        DisplayFallback::ParserBudget => "parser_budget",
        DisplayFallback::DocumentContext => "document_context",
        DisplayFallback::LayoutBudget => "layout_budget",
        DisplayFallback::UnsupportedSource => "unsupported_source",
        DisplayFallback::InvalidBoundary => "invalid_boundary",
        DisplayFallback::ParserFailed => "parser_failed",
        DisplayFallback::IncompleteSource => "incomplete_source",
        DisplayFallback::CacheUnavailable => "cache_unavailable",
    }
}
