//! One bounded reader worker and one writer worker per private SQLite profile.

use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_domain::identity::EventCursor;
use reviewq_domain::session::AbsolutePath;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use tokio::sync::{mpsc, oneshot, watch};

use crate::StoreError;

mod database;
mod profile;
mod staging;
mod wal;

pub use database::StoreIdentity;

const QUEUE_CAPACITY: usize = 64;

type Job = Box<dyn FnOnce(&mut Connection, &profile::Profile) + Send>;

struct Worker {
    sender: Option<mpsc::Sender<Job>>,
    join: Option<JoinHandle<()>>,
}

struct Workers {
    reader: Worker,
    writer: Worker,
}

pub struct SqliteStore {
    identity: StoreIdentity,
    workers: Mutex<Workers>,
    wakeup: watch::Sender<EventCursor>,
}

pub(crate) struct WriteTx<'connection> {
    pub(crate) sql: Transaction<'connection>,
    pub(crate) wakeup: Option<EventCursor>,
}

impl SqliteStore {
    /// Creates or verifies and opens a private profile under its exclusive lock.
    pub async fn open(path: AbsolutePath) -> Result<Self, StoreError> {
        tokio::task::spawn_blocking(move || {
            let (profile, identity, reader, writer) = database::open_profile(&path)?;
            let head = read_head(&reader)?;
            let (wakeup, _) = watch::channel(head);
            let profile = Arc::new(profile);
            let reader = Worker::start(reader, profile.clone(), false)?;
            let writer = Worker::start(writer, profile, true)?;
            Ok(Self {
                identity,
                workers: Mutex::new(Workers { reader, writer }),
                wakeup,
            })
        })
        .await
        .map_err(|_| AppError::Internal)?
    }

    /// Verifies a stopped existing profile without mutating the database or its sidecars.
    pub async fn probe(path: AbsolutePath) -> Result<StoreIdentity, StoreError> {
        tokio::task::spawn_blocking(move || database::probe(&path))
            .await
            .map_err(|_| AppError::Internal)?
    }

    pub fn identity(&self) -> &StoreIdentity {
        &self.identity
    }

    /// Latest commit hint. Consumers recover missed events from the durable journal.
    pub fn subscribe(&self) -> watch::Receiver<EventCursor> {
        self.wakeup.subscribe()
    }

    /// Creates a verified recovery copy at a new private destination.
    pub async fn backup(&self, path: AbsolutePath) -> Result<StoreIdentity, StoreError> {
        self.submit(false, move |connection, source_profile| {
            database::backup_database(connection, source_profile, &path)
        })
        .await
    }

    /// Refuses new work, then drains accepted work and joins both workers to a deadline.
    pub async fn close(&self, timeout: Duration) -> Result<(), StoreError> {
        {
            let mut workers = self.workers.lock().map_err(|_| AppError::Internal)?;
            workers.reader.sender.take();
            workers.writer.sender.take();
        }
        let deadline = Instant::now() + timeout;
        loop {
            let finished = {
                let mut workers = self.workers.lock().map_err(|_| AppError::Internal)?;
                let done = workers.reader.finished() && workers.writer.finished();
                if done {
                    workers.reader.join()?;
                    workers.writer.join()?;
                }
                done
            };
            if finished {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AppError::Busy.into());
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(5))).await;
        }
    }

    pub(crate) async fn read<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T, StoreError> + Send + 'static,
    {
        self.submit(false, move |connection, _| {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let value = f(&tx)?;
            tx.commit()?;
            Ok(value)
        })
        .await
    }

    pub(crate) async fn write<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut WriteTx<'_>) -> Result<T, StoreError> + Send + 'static,
    {
        let wakeup = self.wakeup.clone();
        self.submit(true, move |connection, _| {
            let sql = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut tx = WriteTx { sql, wakeup: None };
            let result = f(&mut tx)?;
            tx.sql.commit()?;
            if let Some(cursor) = tx.wakeup {
                wakeup.send_replace(cursor);
            }
            Ok(result)
        })
        .await
    }

    async fn submit<T, F>(&self, write: bool, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &profile::Profile) -> Result<T, StoreError> + Send + 'static,
    {
        let (response, receiver) = oneshot::channel();
        {
            let workers = self.workers.lock().map_err(|_| AppError::Internal)?;
            let worker = if write {
                &workers.writer
            } else {
                &workers.reader
            };
            let sender = worker.sender.as_ref().ok_or(AppError::Unavailable)?;
            sender
                .try_send(Box::new(move |connection, profile| {
                    let _ = response.send(f(connection, profile));
                }))
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => AppError::Busy,
                    mpsc::error::TrySendError::Closed(_) => AppError::Unavailable,
                })?;
        }
        receiver.await.map_err(|_| AppError::Unavailable)?
    }
}

impl Worker {
    fn start(
        mut connection: Connection,
        profile: Arc<profile::Profile>,
        writer: bool,
    ) -> Result<Self, StoreError> {
        let (sender, mut receiver) = mpsc::channel::<Job>(QUEUE_CAPACITY);
        let name = if writer {
            "reviewq-sqlite-writer"
        } else {
            "reviewq-sqlite-reader"
        };
        let join = thread::Builder::new().name(name.into()).spawn(move || {
            while let Some(job) = receiver.blocking_recv() {
                job(&mut connection, &profile);
            }
            if writer {
                let _ = connection.execute_batch("PRAGMA wal_checkpoint(PASSIVE)");
            }
            drop(connection);
            drop(profile);
        })?;
        Ok(Self {
            sender: Some(sender),
            join: Some(join),
        })
    }

    fn finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn join(&mut self) -> Result<(), StoreError> {
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| AppError::Internal)?;
        }
        Ok(())
    }
}

pub(crate) fn read_head(connection: &Connection) -> Result<EventCursor, StoreError> {
    let value = connection.query_row(
        "SELECT event_head FROM service_state WHERE singleton=1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    EventCursor::try_from(value).map_err(|_| AppError::Internal.into())
}
