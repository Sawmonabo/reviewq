//! SQLite initialization, non-mutating verification, and online backup.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::path::Path;
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_domain::session::AbsolutePath;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, backup};
use tempfile::{Builder, NamedTempFile};
use uuid::Uuid;

use super::profile::Profile;
use crate::StoreError;

const SCHEMA: &str = include_str!("../schema.sql");
pub(super) const APPLICATION_ID: i64 = 0x5256_5132;
pub(super) const FORMAT_ID: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const BACKUP_DEADLINE: Duration = Duration::from_secs(30);
const READ_FLAGS: OpenFlags =
    OpenFlags::SQLITE_OPEN_READ_ONLY.union(OpenFlags::SQLITE_OPEN_NOFOLLOW);
const WRITE_FLAGS: OpenFlags =
    OpenFlags::SQLITE_OPEN_READ_WRITE.union(OpenFlags::SQLITE_OPEN_NOFOLLOW);

/// Private installation store identity obtained from the exact supported schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreIdentity {
    pub application_id: i64,
    pub format: i64,
    pub store_id: Uuid,
}

pub(super) fn open_profile(
    path: &AbsolutePath,
) -> Result<(Profile, StoreIdentity, Connection, Connection), StoreError> {
    let profile = Profile::resolve(path)?;
    profile.try_lock()?;
    let identity = match profile.file("")? {
        Some(database) => inspect_existing(&profile, database)?,
        None => initialize(&profile)?,
    };

    let writer = Connection::open_with_flags(profile.path(), WRITE_FLAGS)?;
    configure(&writer, false)?;
    let mode = writer.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    })?;
    if mode != "wal" {
        return Err(StoreError::path(
            profile.path(),
            "could not enable SQLite WAL mode",
        ));
    }
    writer.pragma_update(None, "synchronous", "FULL")?;
    let reader = Connection::open_with_flags(profile.path(), READ_FLAGS)?;
    configure(&reader, true)?;
    Ok((profile, identity, reader, writer))
}

pub(super) fn probe(path: &AbsolutePath) -> Result<StoreIdentity, StoreError> {
    let profile = Profile::resolve(path)?;
    profile.try_lock()?;
    let database = profile
        .file("")?
        .ok_or_else(|| StoreError::path(profile.path(), "database file does not exist"))?;
    inspect_existing(&profile, database)
}

fn configure(connection: &Connection, read_only: bool) -> Result<(), StoreError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    // SQLite creates WAL/SHM using the already-owner-only database mode.
    // Memory-backed temp storage keeps independent scratch files off disk.
    connection.execute_batch(
        "PRAGMA foreign_keys = ON; PRAGMA trusted_schema = OFF; PRAGMA temp_store = MEMORY;",
    )?;
    if read_only {
        connection.pragma_update(None, "query_only", true)?;
    }
    let enabled: bool = connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    if !enabled {
        return Err(AppError::Internal.into());
    }
    Ok(())
}

fn initialize(profile: &Profile) -> Result<StoreIdentity, StoreError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        if profile.file(suffix)?.is_some() {
            return Err(StoreError::path(
                profile.path(),
                "SQLite sidecars already exist without a database",
            ));
        }
    }
    let temporary = new_staged_file(profile.parent())?;
    let mut connection = Connection::open_with_flags(temporary.path(), WRITE_FLAGS)?;
    configure(&connection, false)?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    let identity = StoreIdentity {
        application_id: APPLICATION_ID,
        format: FORMAT_ID,
        store_id: Uuid::new_v4(),
    };
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "application_id", APPLICATION_ID)?;
    tx.pragma_update(None, "user_version", FORMAT_ID)?;
    tx.execute(
        "INSERT INTO service_state(singleton, store_id) VALUES(1, ?1)",
        [identity.store_id.to_string()],
    )?;
    tx.commit()?;
    drop(connection);
    temporary.as_file().sync_all()?;
    profile.publish(temporary.path())?;
    Ok(identity)
}

fn inspect_existing(profile: &Profile, mut database: File) -> Result<StoreIdentity, StoreError> {
    let original = profile.path();
    let wal = profile.file("-wal")?;
    let shm = profile.file("-shm")?;
    let journal = profile.file("-journal")?;
    let wal_is_retained = wal
        .as_ref()
        .map(File::metadata)
        .transpose()?
        .is_some_and(|meta| meta.len() > 0);
    let mut header = [0u8; 100];
    database.read_exact(&mut header).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            StoreError::path(original, "empty or truncated SQLite file")
        } else {
            error.into()
        }
    })?;
    if &header[..16] != b"SQLite format 3\0" {
        return Err(StoreError::path(original, "not a SQLite database"));
    }
    if i32::from_be_bytes(header[68..72].try_into().expect("application ID field")) as i64
        != APPLICATION_ID
    {
        return Err(StoreError::path(
            original,
            "SQLite application ID belongs to a different format",
        ));
    }
    if journal.is_some() {
        return Err(StoreError::path(
            original,
            "unexpected SQLite rollback journal in the profile",
        ));
    }
    if shm.is_some() && wal.is_none() {
        return Err(StoreError::path(
            original,
            "SQLite shared memory exists without its WAL",
        ));
    }

    let result = if wal_is_retained {
        // An immutable SQLite handle skips committed WAL frames. A normal mode=ro
        // handle can create or modify the WAL index. Let SQLite recover the copy.
        let staging = Builder::new()
            .prefix(".reviewq-probe-")
            .tempdir_in(profile.parent())?;
        let staged_db = staging.path().join("profile.sqlite3");
        let staged_wal = staging.path().join("profile.sqlite3-wal");
        use std::io::Seek as _;
        database.rewind()?;
        copy_stable(database, &staged_db)?;
        copy_stable(wal.expect("retained WAL"), &staged_wal)?;
        let connection = Connection::open_with_flags(staged_db, READ_FLAGS)?;
        verify(&connection, original)
    } else {
        let connection = Connection::open_with_flags(
            immutable_uri(original),
            READ_FLAGS | OpenFlags::SQLITE_OPEN_URI,
        )?;
        verify(&connection, original)
    };
    match result {
        Err(error) if error.is_sqlite() => {
            Err(StoreError::path(original, "SQLite verification failed"))
        }
        result => result,
    }
}

fn immutable_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            write!(uri, "%{byte:02X}").expect("writing to String");
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    uri
}

fn copy_stable(mut source: File, destination: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    let before = source.metadata()?;
    let copied = io::copy(&mut source, &mut output)?;
    let after = source.metadata()?;
    if copied != before.len()
        || before.len() != after.len()
        || before.modified()? != after.modified()?
        || (before.ctime(), before.ctime_nsec()) != (after.ctime(), after.ctime_nsec())
    {
        return Err(AppError::Busy.into());
    }
    Ok(())
}

type SchemaEntry = (String, String, String, Option<String>);

fn table_definitions(connection: &Connection) -> Result<Vec<SchemaEntry>, StoreError> {
    let mut statement = connection
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn verify(connection: &Connection, path: &Path) -> Result<StoreIdentity, StoreError> {
    configure(connection, true)?;
    let application_id = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(StoreError::path(path, "unsupported SQLite application ID"));
    }
    let format = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if format != FORMAT_ID {
        return Err(StoreError::path(path, "unsupported SQLite format version"));
    }
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(SCHEMA)?;
    if table_definitions(connection)? != table_definitions(&expected)? {
        return Err(StoreError::path(
            path,
            "SQLite schema does not match the current build",
        ));
    }
    let result = connection.query_row("PRAGMA integrity_check(1)", [], |row| {
        row.get::<_, String>(0)
    })?;
    if result != "ok"
        || connection
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()?
            .is_some()
    {
        return Err(StoreError::path(
            path,
            "SQLite integrity or foreign-key verification failed",
        ));
    }
    let id = connection.query_row(
        "SELECT store_id FROM service_state WHERE singleton = 1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    let store_id =
        Uuid::parse_str(&id).map_err(|_| StoreError::path(path, "invalid store identity"))?;
    Ok(StoreIdentity {
        application_id,
        format,
        store_id,
    })
}

fn new_staged_file(parent: &Path) -> Result<NamedTempFile, StoreError> {
    Ok(Builder::new().prefix(".reviewq-db-").tempfile_in(parent)?)
}

pub(super) fn backup_database(
    source: &Connection,
    destination: &AbsolutePath,
) -> Result<StoreIdentity, StoreError> {
    let profile = Profile::resolve(destination)?;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        if profile.file(suffix)?.is_some() {
            return Err(StoreError::path(
                profile.path(),
                "backup destination is already occupied",
            ));
        }
    }
    let temporary = new_staged_file(profile.parent())?;
    let mut target = Connection::open_with_flags(temporary.path(), WRITE_FLAGS)?;
    configure(&target, false)?;
    target.pragma_update(None, "synchronous", "FULL")?;
    {
        let backup = backup::Backup::new(source, &mut target)?;
        let deadline = Instant::now() + BACKUP_DEADLINE;
        loop {
            if deadline <= Instant::now() {
                return Err(AppError::Busy.into());
            }
            match backup.step(128)? {
                backup::StepResult::Done => break,
                backup::StepResult::More => {}
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
    target.pragma_update(None, "journal_mode", "DELETE")?;
    let identity = verify(&target, profile.path())?;
    drop(target);
    temporary.as_file().sync_all()?;
    profile.publish(temporary.path())?;
    Ok(identity)
}
