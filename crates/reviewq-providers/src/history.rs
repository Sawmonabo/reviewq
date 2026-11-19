//! Bounded reads of proven native persistence, independent of a running host.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::{
    FileContentRef, HistorySession, NativeContent, NativeHead, NativeHistoryPage,
    NativeHistoryReader, NativeNavigation, NativePlan, NativePosition, NavigationQuery,
    SourceAnchor,
};
use reviewq_domain::identity::NativeInputId;
use tokio::sync::Semaphore;

mod claude;
mod claude_workflow;
mod content;
mod file;
mod identity;
mod locator;
mod navigation;
mod proof;
mod reader;
mod records;
mod task_plan;
pub(crate) mod tools;
mod turn_metadata;
mod watch;

pub(crate) use claude::{claude_anchor, claude_confirm_prompt};
pub(crate) use claude_workflow::{claude_workflow_opening, claude_workflow_result};
pub(crate) use locator::{verified_claude_child, verified_claude_source, verified_codex_source};
pub(crate) use proof::{codex_opening, codex_question_answered, codex_question_opening};
pub(crate) use turn_metadata::{
    codex_exact_turn_settings, codex_turn_settings, codex_turn_started,
};

pub struct FileHistory {
    reads: Arc<Semaphore>,
    streams: content::Streams,
}

impl Default for FileHistory {
    fn default() -> Self {
        Self {
            reads: Arc::new(Semaphore::new(4)),
            streams: content::Streams::default(),
        }
    }
}

pub(crate) struct Cancel(pub(crate) Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl FileHistory {
    fn inspect<T: Send + 'static>(
        &self,
        session: HistorySession,
        inspect: impl FnOnce(locator::Source, &AtomicBool) -> Result<T, AppError> + Send + 'static,
    ) -> AppFuture<'_, Option<T>> {
        Box::pin(async move {
            let permit = self
                .reads
                .clone()
                .try_acquire_owned()
                .map_err(|_| AppError::Busy)?;
            let cancel = Cancel(Arc::new(AtomicBool::new(false)));
            let stopped = cancel.0.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                locator::locate(&session, &stopped)?
                    .map(|source| inspect(source, &stopped))
                    .transpose()
            })
            .await
            .map_err(|_| AppError::Unavailable)?
        })
    }
}

impl NativeHistoryReader for FileHistory {
    fn around(
        &self,
        session: HistorySession,
        id: NativeInputId,
        anchor: Option<SourceAnchor>,
        limit: u16,
    ) -> AppFuture<'_, Option<NativeHistoryPage>> {
        self.inspect(session, move |source, cancel| {
            navigation::around(&source, &id, anchor, limit, cancel)
        })
    }

    fn navigation(
        &self,
        session: HistorySession,
        query: NavigationQuery,
    ) -> AppFuture<'_, Option<NativeNavigation>> {
        self.inspect(session, move |source, cancel| {
            navigation::read(&source, query, cancel)
        })
    }

    fn plan(&self, session: HistorySession) -> AppFuture<'_, Option<NativePlan>> {
        let binding = session.binding.clone();
        self.inspect(session, move |source, cancel| {
            let tasks = task_plan::read(&binding, &source, cancel)?;
            let mut plan = navigation::plan(&source, cancel).or_else(|error| {
                if tasks.is_some() && error == AppError::Unavailable {
                    Ok(NativePlan {
                        generation: file::generation(&source)?,
                        entry: None,
                        tasks: None,
                    })
                } else {
                    Err(error)
                }
            })?;
            plan.tasks = tasks;
            Ok(plan)
        })
    }

    fn watch(
        &self,
        session: HistorySession,
    ) -> AppFuture<'_, Option<Box<dyn reviewq_app::history::NativeHistoryWatch>>> {
        self.inspect(session, move |source, _| watch::watch(source))
    }

    fn head(
        &self,
        session: HistorySession,
        previous: Option<SourceAnchor>,
    ) -> AppFuture<'_, Option<NativeHead>> {
        self.inspect(session, move |source, cancel| {
            navigation::head(&source, previous.as_ref(), cancel)
        })
    }

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
                file::page(&source, position, limit, &stopped).map(Some)
            })
            .await
            .map_err(|_| AppError::Unavailable)?
        })
    }
    fn parts(
        &self,
        session: HistorySession,
        reference: FileContentRef,
        first: u16,
    ) -> AppFuture<'_, reviewq_app::history::NativeParts> {
        Box::pin(async move {
            self.inspect(session, move |source, cancel| {
                file::parts(&source, reference, first, cancel)
            })
            .await?
            .ok_or(AppError::NotFound)
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
            let selected = reference.clone();
            let key = session.target.session_key().clone();
            let source = self
                .inspect(session, move |source, _| {
                    file::verify_content(&source, &selected)?;
                    Ok(source)
                })
                .await?
                .ok_or(AppError::NotFound)?;
            self.streams
                .read(key, reference, source, offset, limit as usize)
                .await
        })
    }
}
