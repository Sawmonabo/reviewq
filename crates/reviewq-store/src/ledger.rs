//! The findings ledger's SQLite storage: each `LedgerStore` call is one short transaction or
//! one read, and attributes its caller inside it.

mod caller;
mod calls;
mod pages;
pub(crate) mod passes;
mod publish;
mod records;
mod rows;
mod settle;
mod snapshot;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::ledger::{
    CallBinding, ClosingRead, FailureRecord, GateAnswer, GateRequest, HoldReceipt, HookSession,
    HostSession, LedgerPage, LedgerRead, LedgerSnapshot, LedgerStore, PublishCommit,
    PublishOutcome, ReadBack, RecordWrite, Recorded, SettleCandidate, WriteRefusal,
};
use reviewq_domain::identity::{ReceiptId, Revision, RunId};
use reviewq_domain::session::HostProvider;
use rusqlite::{Connection, OptionalExtension};

pub(crate) use rows::details_at;

use crate::{SqliteStore, StoreError};

impl LedgerStore for SqliteStore {
    fn write_record(
        &self,
        binding: CallBinding,
        write: RecordWrite,
    ) -> AppFuture<'_, Result<Recorded, WriteRefusal>> {
        Box::pin(async move {
            self.write(move |tx| records::write(tx, &binding, write))
                .await
                .map_err(StoreError::app)
        })
    }

    fn read_ledger(&self, session: HostSession, read: LedgerRead) -> AppFuture<'_, LedgerPage> {
        Box::pin(async move {
            self.read(move |db| pages::read(db, &session, read))
                .await
                .map_err(StoreError::app)
        })
    }

    fn hold_pass(
        &self,
        binding: CallBinding,
        reason: String,
    ) -> AppFuture<'_, Option<HoldReceipt>> {
        Box::pin(async move {
            self.write(move |tx| passes::hold(tx, &binding, &reason))
                .await
                .map_err(StoreError::app)
        })
    }

    fn answer_gate(&self, request: GateRequest) -> AppFuture<'_, GateAnswer> {
        Box::pin(async move {
            self.write(move |tx| passes::answer(&tx.sql, &request))
                .await
                .map_err(StoreError::app)
        })
    }

    fn closing_snapshot(&self, binding: CallBinding) -> AppFuture<'_, ClosingRead> {
        Box::pin(async move {
            self.read(move |db| snapshot::closing(db, &binding))
                .await
                .map_err(StoreError::app)
        })
    }

    fn update_snapshot(&self, run: RunId) -> AppFuture<'_, LedgerSnapshot> {
        Box::pin(async move {
            self.read(move |db| snapshot::update(db, &run))
                .await
                .map_err(StoreError::app)
        })
    }

    fn commit_publish(&self, commit: PublishCommit) -> AppFuture<'_, PublishOutcome> {
        Box::pin(async move {
            self.write(move |tx| publish::commit(tx, commit))
                .await
                .map_err(StoreError::app)
        })
    }

    fn index_version(&self, run: RunId, receipt: ReceiptId) -> AppFuture<'_, ()> {
        Box::pin(async move {
            publish::index(self, run, receipt)
                .await
                .map_err(StoreError::app)
        })
    }

    fn runs_to_settle(&self, run: Option<RunId>) -> AppFuture<'_, Vec<SettleCandidate>> {
        Box::pin(async move {
            self.read(move |db| settle::candidates(db, run.as_ref()))
                .await
                .map_err(StoreError::app)
        })
    }

    fn record_failure(&self, failure: FailureRecord) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| settle::record_failure(tx, &failure))
                .await
                .map_err(StoreError::app)
        })
    }

    fn read_back(&self, session: HookSession) -> AppFuture<'_, Option<ReadBack>> {
        Box::pin(async move {
            self.read(move |db| passes::read_back(db, &session))
                .await
                .map_err(StoreError::app)
        })
    }

    fn last_stop_hook_report(&self, provider: HostProvider) -> AppFuture<'_, Option<i64>> {
        Box::pin(async move {
            self.read(move |db| passes::last_stop_hook_report(db, provider))
                .await
                .map_err(StoreError::app)
        })
    }
}

/// The run's ledger sequence: 0 before its first accepted change.
fn sequence(db: &Connection, run: &str) -> Result<Revision, StoreError> {
    let value: Option<i64> = db
        .prepare_cached("SELECT sequence FROM ledger_runs WHERE run_id=?1")?
        .query_row([run], |row| row.get(0))
        .optional()?;
    Revision::try_from(value.unwrap_or(0)).map_err(|_| AppError::Internal.into())
}

/// The one count `sql` selects for `run`.
fn count(db: &Connection, sql: &str, run: &str) -> Result<u32, StoreError> {
    let value: i64 = db.prepare_cached(sql)?.query_row([run], |row| row.get(0))?;
    u32::try_from(value).map_err(|_| AppError::Internal.into())
}

/// Records of the run with a change no published version shows yet.
fn unpublished(db: &Connection, run: &str) -> Result<u32, StoreError> {
    count(
        db,
        "SELECT (SELECT count(*) FROM ledger_findings f WHERE f.run_id=l.run_id AND f.sequence>l.published_sequence)
              + (SELECT count(*) FROM ledger_notes n WHERE n.run_id=l.run_id AND n.sequence>l.published_sequence)
         FROM (SELECT ?1 AS run_id) x LEFT JOIN ledger_runs l USING(run_id)",
        run,
    )
}

/// The run's active findings.
fn active_findings(db: &Connection, run: &str) -> Result<u32, StoreError> {
    count(
        db,
        "SELECT count(*) FROM ledger_findings WHERE run_id=?1 AND active=1",
        run,
    )
}

#[cfg(test)]
mod tests;
