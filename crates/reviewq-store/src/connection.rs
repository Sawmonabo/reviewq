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
mod display_cache;
mod profile;
mod staging;
mod wal;

pub use database::StoreIdentity;

const QUEUE_CAPACITY: usize = 64;

type Job = Box<dyn FnOnce(&mut Connection, &Arc<profile::Profile>) + Send>;

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
    delivery_wakeup: watch::Sender<()>,
    report_wakeup: watch::Sender<()>,
    desktop_wakeup: watch::Sender<()>,
    projection_wakeup: watch::Sender<()>,
}

pub(crate) struct WriteTx<'connection> {
    pub(crate) sql: Transaction<'connection>,
    pub(crate) wakeup: Option<EventCursor>,
    pub(crate) delivery_changed: bool,
    pub(crate) report_changed: bool,
    pub(crate) desktop_changed: bool,
    pub(crate) projection_changed: bool,
}

impl SqliteStore {
    /// Creates or verifies and opens a private profile under its exclusive lock.
    pub async fn open(path: AbsolutePath) -> Result<Self, StoreError> {
        tokio::task::spawn_blocking(move || {
            let (profile, identity, reader, writer) = database::open_profile(&path)?;
            let head = read_head(&reader)?;
            let (wakeup, _) = watch::channel(head);
            let (delivery_wakeup, _) = watch::channel(());
            let (report_wakeup, _) = watch::channel(());
            let (desktop_wakeup, _) = watch::channel(());
            let (projection_wakeup, _) = watch::channel(());
            let profile = Arc::new(profile);
            let reader = Worker::start(reader, profile.clone(), false)?;
            let writer = Worker::start(writer, profile, true)?;
            Ok(Self {
                identity,
                workers: Mutex::new(Workers { reader, writer }),
                wakeup,
                delivery_wakeup,
                report_wakeup,
                desktop_wakeup,
                projection_wakeup,
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

    /// Wake the delivery dispatcher only after a committed outbox change.
    pub fn subscribe_deliveries(&self) -> watch::Receiver<()> {
        self.delivery_wakeup.subscribe()
    }

    /// Wake the capture worker after a new report handoff is committed.
    pub fn subscribe_reports(&self) -> watch::Receiver<()> {
        self.report_wakeup.subscribe()
    }

    pub fn subscribe_desktop(&self) -> watch::Receiver<()> {
        self.desktop_wakeup.subscribe()
    }

    pub fn subscribe_checkout_projections(&self) -> watch::Receiver<()> {
        self.projection_wakeup.subscribe()
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
        self.write_prepared(|_| Ok(()), move |tx, ()| f(tx)).await
    }

    /// Prepares connection-local scratch before acquiring the persistent write lock.
    pub(crate) async fn write_prepared<T, P, S, F>(&self, prepare: S, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        S: FnOnce(&mut Connection) -> Result<P, StoreError> + Send + 'static,
        F: FnOnce(&mut WriteTx<'_>, P) -> Result<T, StoreError> + Send + 'static,
    {
        let wakeup = self.wakeup.clone();
        let delivery_wakeup = self.delivery_wakeup.clone();
        let report_wakeup = self.report_wakeup.clone();
        let desktop_wakeup = self.desktop_wakeup.clone();
        let projection_wakeup = self.projection_wakeup.clone();
        self.submit(true, move |connection, _| {
            let prepared = prepare(connection)?;
            let sql = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut tx = WriteTx {
                sql,
                wakeup: None,
                delivery_changed: false,
                report_changed: false,
                desktop_changed: false,
                projection_changed: false,
            };
            let result = f(&mut tx, prepared)?;
            tx.sql.commit()?;
            if let Some(cursor) = tx.wakeup {
                wakeup.send_replace(cursor);
            }
            if tx.delivery_changed {
                delivery_wakeup.send_replace(());
            }
            if tx.report_changed {
                report_wakeup.send_replace(());
            }
            if tx.desktop_changed {
                desktop_wakeup.send_replace(());
            }
            if tx.projection_changed {
                projection_wakeup.send_replace(());
            }
            Ok(result)
        })
        .await
    }

    async fn submit<T, F>(&self, write: bool, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &Arc<profile::Profile>) -> Result<T, StoreError>
            + Send
            + 'static,
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
                    if write || !response.is_closed() {
                        let _ = response.send(f(connection, profile));
                    }
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
