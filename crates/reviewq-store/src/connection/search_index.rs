//! The conversation index is a separate, process-disposable SQLite derivative.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::access::{AppError, admits_disk_scratch};
use reviewq_app::history::search::index::{CandidateIndex, CandidateWriter, SearchIndex};
use reviewq_domain::session::AbsolutePath;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::profile::Profile;
use super::staging::{self, Stage};

const PAGE_BYTES: i64 = 4096;
const MAX_PAGES: i64 = 8192;
const AGE: Duration = Duration::from_secs(120);

const SCHEMA: &str = "
CREATE VIRTUAL TABLE postings USING fts5(folded, content='', columnsize=0,
  detail=none, tokenize='trigram case_sensitive 1');
CREATE TABLE locators (rowid INTEGER PRIMARY KEY, record INTEGER NOT NULL) STRICT;
CREATE TABLE coverage (start INTEGER PRIMARY KEY, first INTEGER NOT NULL,
  last INTEGER NOT NULL, certified INTEGER NOT NULL) STRICT;
CREATE INDEX gaps ON coverage(start) WHERE certified=0;
";

pub(super) struct Indexes {
    profile: PathBuf,
    slot: Arc<Mutex<Slot>>,
}

#[derive(Default)]
struct Slot {
    building: bool,
    ready: Option<(Instant, Arc<Published>)>,
}

struct Published {
    key: [u8; 32],
    end: u64,
    stage: Stage,
}

impl Indexes {
    pub(super) fn new(profile: &Profile) -> Self {
        let slot = Arc::new(Mutex::new(Slot::default()));
        let owner = Arc::downgrade(&slot);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let Some(slot) = owner.upgrade() else { break };
                if let Ok(mut slot) = slot.try_lock() {
                    expire(&mut slot);
                }
            }
        });
        Self {
            profile: profile.path().to_owned(),
            slot,
        }
    }
}

fn expire(slot: &mut Slot) {
    if slot
        .ready
        .as_ref()
        .is_some_and(|(used, ready)| used.elapsed() >= AGE && Arc::strong_count(ready) == 1)
    {
        slot.ready.take();
    }
}

impl SearchIndex for Indexes {
    fn reader(&self, key: [u8; 32], trigram: &str) -> Option<Box<dyn CandidateIndex>> {
        let published = {
            let mut slot = self.slot.lock().ok()?;
            expire(&mut slot);
            let (used, ready) = slot.ready.as_mut()?;
            if ready.key != key {
                return None;
            }
            *used = Instant::now();
            ready.clone()
        };
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let db = Connection::open_with_flags(&published.stage.database, flags).ok()?;
        db.execute_batch(
            "PRAGMA query_only=ON; PRAGMA cache_size=-2048; PRAGMA temp_store=MEMORY;",
        )
        .ok()?;
        Some(Box::new(Lookup {
            db,
            published,
            term: format!("\"{}\"", trigram.replace('"', "\"\"")),
        }))
    }

    fn writer(&self, key: [u8; 32]) -> Result<Box<dyn CandidateWriter>, AppError> {
        let claim = {
            let mut slot = self.slot.lock().map_err(|_| AppError::Unavailable)?;
            expire(&mut slot);
            if slot.building
                || slot
                    .ready
                    .as_ref()
                    .is_some_and(|(_, ready)| ready.key == key || Arc::strong_count(ready) > 1)
            {
                return Err(AppError::Busy);
            }
            // Retain at most one database, including every open reader and build.
            slot.ready.take();
            slot.building = true;
            Claim(self.slot.clone())
        };
        let path = AbsolutePath::from_canonical(self.profile.clone())
            .map_err(|_| AppError::Unavailable)?;
        let profile = Profile::resolve(&path).map_err(|_| AppError::Unavailable)?;
        let stage = Stage::new(&profile).map_err(|_| AppError::Unavailable)?;
        let file = staging::create_private(&stage.database).map_err(|_| AppError::Unavailable)?;
        let fs = rustix::fs::fstatfs(&file)
            .map_err(|_| AppError::Unavailable)?
            .f_type as u64;
        if !matches!(
            fs,
            0xef53 | 0x5846_5342 | 0x9123_683e | 0xf2f5_2010 | 0x2fc1_2fc1
        ) {
            return Err(AppError::Unavailable);
        }
        let db = Connection::open_with_flags(
            &stage.database,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(error)?;
        db.pragma_update(None, "page_size", PAGE_BYTES)
            .map_err(error)?;
        db.pragma_update(None, "max_page_count", MAX_PAGES)
            .map_err(error)?;
        let mode: String = db
            .query_row("PRAGMA journal_mode=MEMORY", [], |row| row.get(0))
            .map_err(error)?;
        let pages: i64 = db
            .pragma_query_value(None, "max_page_count", |row| row.get(0))
            .map_err(error)?;
        if mode != "memory" || pages != MAX_PAGES {
            return Err(AppError::Unavailable);
        }
        db.execute_batch("PRAGMA cache_size=-2048; PRAGMA temp_store=MEMORY;")
            .map_err(error)?;
        db.execute_batch(SCHEMA).map_err(error)?;
        Ok(Box::new(Build {
            key,
            claim,
            stage,
            db,
            next_row: 1,
            pending: None,
            covered: 0,
        }))
    }
}

struct Claim(Arc<Mutex<Slot>>);

impl Drop for Claim {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.0.lock() {
            slot.building = false;
        }
    }
}

struct Build {
    key: [u8; 32],
    claim: Claim,
    stage: Stage,
    db: Connection,
    next_row: i64,
    pending: Option<(i64, i64)>,
    covered: u64,
}

impl CandidateWriter for Build {
    fn begin_record(&mut self, start: u64) -> Result<(), AppError> {
        if self.pending.is_some() || self.covered != start {
            return Err(AppError::Internal);
        }
        self.db.execute_batch("BEGIN").map_err(error)?;
        self.pending = Some((
            i64::try_from(start).map_err(|_| AppError::InvalidRequest)?,
            self.next_row,
        ));
        Ok(())
    }

    fn folded_chunk(&mut self, folded: &str) -> Result<(), AppError> {
        let (start, _) = self.pending.ok_or(AppError::Internal)?;
        self.db
            .execute(
                "INSERT INTO postings(rowid,folded) VALUES(?1,?2)",
                params![self.next_row, folded],
            )
            .map_err(error)?;
        self.db
            .execute(
                "INSERT INTO locators(rowid,record) VALUES(?1,?2)",
                params![self.next_row, start],
            )
            .map_err(error)?;
        self.next_row += 1;
        Ok(())
    }

    fn commit_record(&mut self, end: u64, certified: bool) -> Result<(), AppError> {
        let (start, first) = self.pending.ok_or(AppError::Internal)?;
        if i64::try_from(end).map_err(|_| AppError::InvalidRequest)? <= start {
            return Err(AppError::Internal);
        }
        self.db
            .execute(
                "INSERT INTO coverage(start,first,last,certified) VALUES(?1,?2,?3,?4)",
                params![start, first, self.next_row - 1, certified],
            )
            .map_err(error)?;
        self.db.execute_batch("COMMIT").map_err(error)?;
        self.pending = None;
        self.covered = end;
        Ok(())
    }

    fn publish(self: Box<Self>) -> Result<(), AppError> {
        let Self {
            key,
            claim,
            stage,
            db,
            covered,
            ..
        } = *self;
        if !db.is_autocommit() {
            db.execute_batch("ROLLBACK").map_err(error)?;
        }
        db.close().map_err(|(_, problem)| error(problem))?;
        let mut slot = claim.0.lock().map_err(|_| AppError::Unavailable)?;
        slot.ready = Some((
            Instant::now(),
            Arc::new(Published {
                key,
                end: covered,
                stage,
            }),
        ));
        Ok(())
    }
}

struct Lookup {
    db: Connection,
    published: Arc<Published>,
    term: String,
}

impl CandidateIndex for Lookup {
    fn covered_through(&self) -> u64 {
        self.published.end
    }

    fn next(&self, record: u64, reverse: bool) -> Result<Option<u64>, AppError> {
        let record = i64::try_from(record).map_err(|_| AppError::InvalidRequest)?;
        let (edge, bound, sort) = if reverse {
            ("last", "<=", "DESC")
        } else {
            ("first", ">=", "ASC")
        };
        let row = self.db.query_row(&format!("SELECT {edge} FROM coverage WHERE start {bound} ?1 AND first<=last ORDER BY start {sort} LIMIT 1"),
            [record], |row| row.get::<_, i64>(0)).optional().map_err(error)?;
        let candidate = if let Some(row) = row {
            let found = self.db.query_row(&format!("SELECT rowid FROM postings WHERE postings MATCH ?1 AND rowid {bound} ?2 ORDER BY rowid {sort} LIMIT 1"),
                params![self.term,row], |row| row.get::<_, i64>(0)).optional().map_err(error)?;
            found
                .map(|row| {
                    self.db
                        .query_row("SELECT record FROM locators WHERE rowid=?1", [row], |row| {
                            row.get::<_, i64>(0)
                        })
                })
                .transpose()
                .map_err(error)?
        } else {
            None
        };
        let gap = self.db.query_row(&format!("SELECT start FROM coverage WHERE certified=0 AND start {bound} ?1 ORDER BY start {sort} LIMIT 1"),
            [record], |row| row.get::<_, i64>(0)).optional().map_err(error)?;
        let next = match (candidate, gap) {
            (Some(hit), Some(gap)) if reverse => Some(hit.max(gap)),
            (Some(hit), Some(gap)) => Some(hit.min(gap)),
            (hit, gap) => hit.or(gap),
        };
        next.map(|value| u64::try_from(value).map_err(|_| AppError::Unavailable))
            .transpose()
    }
}

fn error(problem: rusqlite::Error) -> AppError {
    if problem.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
        AppError::PayloadTooLarge
    } else {
        AppError::Unavailable
    }
}
