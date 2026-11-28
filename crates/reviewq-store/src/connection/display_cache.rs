//! Optional display metadata lives beside the authoritative database.

mod layout;
mod window;

pub(super) use window::Builds;

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::reports::{DisplayFallback, DisplayIndex};
use reviewq_domain::identity::{ReceiptId, RunId};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::profile::Profile;
use super::staging::{self, Stage};
use crate::reports::{read_markdown, version};
use crate::{SqliteStore, StoreError};

const SUFFIX: &str = ".findings-display";
const APPLICATION_ID: i64 = 0x5256_4644;
const SCHEMA_VERSION: i64 = 3;
/// What SQLite may leave beside the cache file.
const LEFTOVERS: [&str; 3] = ["-journal", "-wal", "-shm"];
const HEAL_INTERVAL: Duration = Duration::from_secs(10 * 60);
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
    lead_markdown TEXT CHECK (length(CAST(lead_markdown AS BLOB))<=1024),
    checksum BLOB NOT NULL CHECK (length(checksum)=32),
    CHECK ((kind='heading')=(heading_level IS NOT NULL)),
    PRIMARY KEY (entry_id, byte_start)
) STRICT, WITHOUT ROWID;
";

impl SqliteStore {
    /// Builds the display index of a published version from its stored text, under the
    /// reader's document budget. A version over the budget is left unindexed, because the
    /// reader answers it with the `document_budget` fallback without consulting the cache.
    /// `NotFound` unless the version exists.
    pub(crate) async fn cache_report_display(
        &self,
        run: RunId,
        receipt: ReceiptId,
    ) -> Result<(), StoreError> {
        // Read the text on the normal reader and release its queue slot before opening or
        // writing the independent cache.
        let source = self
            .submit(false, move |db, profile| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
                let version = version(&tx, &run, &receipt)?;
                if version.summary.stats.require_markdown_budget().is_err() {
                    return Ok(None);
                }
                let source = read_markdown(&tx, &version)?;
                tx.commit()?;
                Ok(Some((
                    profile.clone(),
                    version.artifact_id,
                    version.summary.reference.hash,
                    source,
                )))
            })
            .await?;
        let Some((profile, artifact_id, hash, source)) = source else {
            return Ok(());
        };
        self.build_published_index(profile, artifact_id, hash, source)
            .await
    }
}

/// The one owner of discarding the cache file. A file this build cannot use (one written
/// for another schema or store, one SQLite cannot read, or WAL files ReviewQ never writes)
/// is replaced by a fresh cache that has already passed the same check, so nothing is
/// discarded unless a usable cache takes its place. The swap happens at most once per
/// `HEAL_INTERVAL`, so a file that keeps failing costs one rebuild per interval and otherwise
/// answers `cache_unavailable`. Only the four cache file names are ever touched, never the
/// main database.
///
/// Every open cache connection holds the lock for reading, and a swap holds it for writing,
/// so no connection to the old file is left to write a journal beside the new one. The
/// standard lock on Linux prefers a waiting writer: a swap waits only for the connections
/// already open, and connections asked for meanwhile wait for the swap.
#[derive(Default)]
pub(super) struct Heal {
    last_replaced: RwLock<Option<Instant>>,
}

/// An open cache connection, holding `Heal` for reading until it closes.
pub(super) struct Cache<'a> {
    connection: Connection,
    _held: RwLockReadGuard<'a, Option<Instant>>,
}

impl Deref for Cache<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.connection
    }
}

impl DerefMut for Cache<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

enum Check {
    Usable(Connection),
    Unusable,
}

fn open<'a>(
    profile: &Profile,
    store_id: Uuid,
    heal: &'a Heal,
    create: bool,
) -> Result<Option<Cache<'a>>, StoreError> {
    let held = heal.read()?;
    match inspect(profile, store_id)? {
        Some(Check::Usable(connection)) => {
            return Ok(Some(Cache {
                connection,
                _held: held,
            }));
        }
        None if !create => return Ok(None),
        _ => {}
    }
    drop(held);
    heal.replace(profile, store_id, false)?;
    let held = heal.read()?;
    match inspect(profile, store_id)? {
        Some(Check::Usable(connection)) => Ok(Some(Cache {
            connection,
            _held: held,
        })),
        _ => Err(AppError::Conflict.into()),
    }
}

impl Heal {
    /// A read or write that finds the open cache damaged discards it like a foreign file.
    pub(super) fn after_failure(&self, profile: &Profile, store_id: Uuid, error: &StoreError) {
        if matches!(
            error.sqlite_code(),
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
        ) {
            let _ = self.replace(profile, store_id, true);
        }
    }

    fn read(&self) -> Result<RwLockReadGuard<'_, Option<Instant>>, StoreError> {
        Ok(self.last_replaced.read().map_err(|_| AppError::Internal)?)
    }

    /// `damaged` replaces a file that passed `inspect` but failed a later read or write. The
    /// caller must not hold a `Cache`, which would wait on itself.
    fn replace(&self, profile: &Profile, store_id: Uuid, damaged: bool) -> Result<(), StoreError> {
        let mut last = self.last_replaced.write().map_err(|_| AppError::Internal)?;
        // Another caller may have created or replaced the file while this one waited.
        let found = match inspect(profile, store_id)? {
            Some(Check::Usable(_)) if !damaged => return Ok(()),
            found => found.is_some(),
        };
        if found && last.is_some_and(|at| at.elapsed() < HEAL_INTERVAL) {
            return Err(AppError::Conflict.into());
        }
        let stage = Stage::new(profile)?;
        create(&stage.database, store_id)?;
        let Check::Usable(staged) = verify(&stage.database, store_id)? else {
            return Err(AppError::Conflict.into());
        };
        drop(staged);
        if found {
            *last = Some(Instant::now());
            // A stale journal beside the fresh file would be rolled back into it.
            for leftover in LEFTOVERS {
                profile.remove_sibling(&format!("{SUFFIX}{leftover}"))?;
            }
        }
        profile.replace_sibling(&stage.database, SUFFIX)?;
        Ok(())
    }
}

/// `None` when there is no cache and nothing left of one.
fn inspect(profile: &Profile, store_id: Uuid) -> Result<Option<Check>, StoreError> {
    let exists = profile.file(SUFFIX)?.is_some();
    let journal = profile.file(&format!("{SUFFIX}-journal"))?.is_some();
    // A journal beside an existing cache is SQLite's own crash recovery.
    if (!exists && journal)
        || profile.file(&format!("{SUFFIX}-wal"))?.is_some()
        || profile.file(&format!("{SUFFIX}-shm"))?.is_some()
    {
        return Ok(Some(Check::Unusable));
    }
    if !exists {
        return Ok(None);
    }
    verify(&cache_path(profile), store_id).map(Some)
}

fn cache_path(profile: &Profile) -> PathBuf {
    let mut path = profile.path().as_os_str().to_os_string();
    path.push(SUFFIX);
    PathBuf::from(path)
}

fn create(path: &Path, store_id: Uuid) -> Result<(), StoreError> {
    let file = staging::create_private(path)?;
    let mut connection = Connection::open_with_flags(path, WRITE_FLAGS)?;
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
    Ok(())
}

/// Whether the file is exactly the cache this build creates for this store. Busy, I/O and
/// other failures are errors, so a passing moment never discards a good cache.
fn verify(path: &Path, store_id: Uuid) -> Result<Check, StoreError> {
    let connection = Connection::open_with_flags(path, WRITE_FLAGS)?;
    match matches_build(&connection, store_id) {
        Ok(true) => {}
        Ok(false) => return Ok(Check::Unusable),
        Err(error)
            if matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
            ) =>
        {
            return Ok(Check::Unusable);
        }
        Err(error) => return Err(error.into()),
    }
    connection.pragma_update(None, "max_page_count", MAX_PAGES)?;
    let page_limit =
        connection.pragma_query_value(None, "max_page_count", |row| row.get::<_, i64>(0))?;
    if page_limit != MAX_PAGES {
        return Err(AppError::Conflict.into());
    }
    Ok(Check::Usable(connection))
}

fn matches_build(connection: &Connection, store_id: Uuid) -> rusqlite::Result<bool> {
    connection.busy_timeout(Duration::from_millis(150))?;
    connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON;")?;
    let pragma = |name: &str| connection.pragma_query_value(None, name, |row| row.get::<_, i64>(0));
    if pragma("application_id")? != APPLICATION_ID
        || pragma("user_version")? != SCHEMA_VERSION
        || pragma("page_size")? != PAGE_BYTES
        || connection.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?
            != "delete"
        || schema_rows(connection)? != *expected_schema()?
    {
        return Ok(false);
    }
    let owner = connection
        .query_row(
            "SELECT store_id FROM cache_owner WHERE singleton=1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(owner == Some(store_id.to_string()))
}

type SchemaRow = (String, String, String, Option<String>);

fn schema_rows(connection: &Connection) -> rusqlite::Result<Vec<SchemaRow>> {
    let mut statement = connection
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")?;
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect()
}

/// The tables `SCHEMA` creates, read back once from a scratch database in memory.
fn expected_schema() -> rusqlite::Result<&'static Vec<SchemaRow>> {
    static EXPECTED: OnceLock<Vec<SchemaRow>> = OnceLock::new();
    if let Some(rows) = EXPECTED.get() {
        return Ok(rows);
    }
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(SCHEMA)?;
    let rows = schema_rows(&connection)?;
    Ok(EXPECTED.get_or_init(|| rows))
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
        DisplayFallback::CacheUnavailable => "cache_unavailable",
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use reviewq_app::reports::build_report_display_index;
    use reviewq_domain::session::AbsolutePath;
    use rusqlite::Connection;
    use uuid::Uuid;

    use super::{Heal, Profile, WRITE_FLAGS, open, publish, staging};

    /// A cache file with this build's version but another table layout, or one SQLite cannot
    /// read, is replaced by a working cache once; a second unusable file inside the interval is left alone; and a
    /// file this build made is kept as it is.
    #[test]
    fn an_unusable_cache_file_is_replaced_once_and_a_current_one_is_kept() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let database = temp.path().join("cache.sqlite3");
        let profile =
            Profile::resolve(&AbsolutePath::from_canonical(database.clone()).unwrap()).unwrap();
        let cache = temp.path().join("cache.sqlite3.findings-display");
        let store_id = Uuid::new_v4();
        let current = super::SCHEMA.replace("lead_markdown", "heading_markdown");
        staging::create_private(&cache).unwrap();
        let old = Connection::open_with_flags(&cache, WRITE_FLAGS).unwrap();
        old.execute_batch(&format!(
            "PRAGMA page_size=4096; {current} PRAGMA application_id=1381385796;
             PRAGMA user_version={};
             INSERT INTO cache_owner(singleton, store_id) VALUES(1, '{store_id}');",
            super::SCHEMA_VERSION
        ))
        .unwrap();
        drop(old);

        let heal = Heal::default();
        let mut replaced = open(&profile, store_id, &heal, false).unwrap().unwrap();
        let source = "# Report\n\n### F-01 · One\n\n**P1 · Bug · Blocking**\n\nText.\n";
        let index = build_report_display_index(source);
        publish(&mut replaced, "artifact", &"a".repeat(64), 3, index).unwrap();
        let spans: i64 = replaced
            .query_row(
                "SELECT count(*) FROM display_spans WHERE lead_markdown IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(spans > 0);
        drop(replaced);
        let inode = std::fs::metadata(&cache).unwrap().ino();
        drop(
            open(&profile, store_id, &Heal::default(), false)
                .unwrap()
                .unwrap(),
        );
        assert_eq!(std::fs::metadata(&cache).unwrap().ino(), inode);

        std::fs::write(&cache, b"not a database ".repeat(400)).unwrap();
        assert!(open(&profile, store_id, &heal, true).is_err());
        assert!(
            open(&profile, store_id, &Heal::default(), true)
                .unwrap()
                .is_some()
        );
    }
}
