//! Bounded reads of proven native persistence, independent of a running host.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::{
    FileContentRef, HistorySession, NativeContent, NativeHistoryPage, NativeHistoryReader,
    NativePosition,
};
use tokio::sync::Semaphore;

mod file;
mod identity;
mod locator;
mod reader;
mod records;

pub(crate) use locator::verified_codex_source;

pub struct FileHistory {
    reads: Arc<Semaphore>,
}

impl Default for FileHistory {
    fn default() -> Self {
        Self {
            reads: Arc::new(Semaphore::new(4)),
        }
    }
}

struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl NativeHistoryReader for FileHistory {
    fn page(
        &self,
        session: HistorySession,
        position: NativePosition,
        limit: u16,
    ) -> AppFuture<'_, Option<NativeHistoryPage>> {
        Box::pin(async move {
            if !(1..=100).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            let permit = self
                .reads
                .clone()
                .try_acquire_owned()
                .map_err(|_| AppError::Busy)?;
            let cancel = Cancel(Arc::new(AtomicBool::new(false)));
            let stopped = cancel.0.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let Some(source) = locator::locate(&session, &stopped)? else {
                    return Ok(None);
                };
                file::page(source, position, limit, &stopped).map(Some)
            })
            .await
            .map_err(|_| AppError::Unavailable)?
        })
    }
    fn content(
        &self,
        session: HistorySession,
        reference: FileContentRef,
        offset: u64,
        limit: u32,
    ) -> AppFuture<'_, NativeContent> {
        Box::pin(async move {
            if !(4..=65_536).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            let permit = self
                .reads
                .clone()
                .try_acquire_owned()
                .map_err(|_| AppError::Busy)?;
            let cancel = Cancel(Arc::new(AtomicBool::new(false)));
            let stopped = cancel.0.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let source = locator::locate(&session, &stopped)?.ok_or(AppError::NotFound)?;
                file::content(source, reference, offset, limit, &stopped)
            })
            .await
            .map_err(|_| AppError::Unavailable)?
        })
    }
}
