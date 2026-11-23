//! Bounded reads of proven native persistence, independent of a running host.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::search::{HistorySearch, SearchPage, SourceObservation};
use reviewq_app::history::{
    FileContentRef, HistorySession, NativeContent, NativeFinalResponse, NativeHead,
    NativeHistoryPage, NativeHistoryReader, NativeNavigation, NativePlan, NativePosition,
    NavigationQuery, SourceAnchor,
};
use reviewq_app::hosts::{ObservedAsyncQuestion, QuestionForm};
use reviewq_domain::identity::NativeInputId;
use tokio::sync::Semaphore;

mod claude;
pub(crate) mod claude_questions;
mod claude_workflow;
mod codex_report;
mod content;
mod documents;
mod file;
mod identity;
mod locator;
mod native_string;
mod navigation;
mod proof;
mod questions;
mod reader;
mod records;
mod search;
mod task_plan;
pub(crate) mod tools;
mod turn_metadata;
mod wait;
mod watch;

pub(crate) use claude::{claude_anchor, claude_confirm_prompt};
pub(crate) use claude_workflow::{claude_workflow_opening, claude_workflow_result};
pub(crate) use codex_report::{codex_earlier_report_failures, codex_report_terminal};
pub(crate) use locator::{
    inspect_claude_lead, verified_claude_child, verified_claude_source, verified_codex_source,
    verify_codex_live_source,
};
pub(crate) use proof::{codex_opening, codex_question_answered, codex_question_opening};
pub(crate) use turn_metadata::{
    codex_exact_turn_settings, codex_turn_settings, codex_turn_started,
};

pub struct FileHistory {
    reads: Arc<Semaphore>,
    searches: Semaphore,
    pending_searches: Semaphore,
    large_records: Arc<Mutex<search::LargeRecords>>,
    search_index: Option<Arc<search::Acceleration>>,
    streams: content::Streams,
    documents: Arc<documents::Documents>,
}

impl Default for FileHistory {
    fn default() -> Self {
        Self {
            reads: Arc::new(Semaphore::new(4)),
            searches: Semaphore::new(1),
            pending_searches: Semaphore::new(4),
            large_records: Arc::default(),
            search_index: None,
            streams: content::Streams::default(),
            documents: Arc::default(),
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
    pub fn with_search_index(
        mut self,
        index: Arc<dyn reviewq_app::history::search::index::SearchIndex>,
    ) -> Self {
        self.search_index = Some(Arc::new(search::Acceleration::new(index)));
        self
    }

    /// Keep an active large display body seekable in the private service profile.
    pub fn with_search_profile(directory: PathBuf) -> Self {
        let large_records = Arc::new(Mutex::new(search::LargeRecords::new(directory)));
        let owner = Arc::downgrade(&large_records);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let Some(records) = owner.upgrade() else {
                    break;
                };
                if let Ok(mut records) = records.try_lock() {
                    records.expire();
                }
            }
        });
        Self {
            large_records,
            ..Self::default()
        }
    }

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

    fn extend_overview(
        &self,
        session: HistorySession,
        checkpoint: reviewq_app::history::PromptOverviewCheckpoint,
        pinned_start: Option<reviewq_app::history::FilePosition>,
    ) -> AppFuture<'_, Option<reviewq_app::history::PromptOverviewCheckpoint>> {
        self.inspect(session, move |source, cancel| {
            navigation::extend(&source, checkpoint, pinned_start, cancel)
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

    fn final_response(
        &self,
        session: HistorySession,
    ) -> AppFuture<'_, Option<NativeFinalResponse>> {
        self.inspect(session, move |source, cancel| {
            file::final_response(&source, cancel)
        })
    }

    fn async_questions(
        &self,
        session: HistorySession,
        known: Vec<ObservedAsyncQuestion>,
        request: Option<String>,
    ) -> AppFuture<'_, Vec<QuestionForm>> {
        Box::pin(async move {
            let unavailable: Vec<_> = known.iter().map(questions::unverified).collect();
            match self
                .inspect(session, move |source, cancel| {
                    questions::read(&source, &known, request.as_deref(), cancel)
                })
                .await
            {
                Ok(Some(forms)) => Ok(forms),
                Ok(None) => Ok(unavailable),
                Err(_) if !unavailable.is_empty() => Ok(unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn prove_async_question(
        &self,
        session: HistorySession,
        evidence: ObservedAsyncQuestion,
    ) -> AppFuture<'_, Option<QuestionForm>> {
        self.inspect(session, move |source, cancel| {
            questions::prove(&source, &evidence, cancel)
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

    fn document(
        &self,
        session: HistorySession,
        reference: FileContentRef,
        query: reviewq_app::history::document::Query,
    ) -> AppFuture<'_, reviewq_app::history::document::Window> {
        Box::pin(async move {
            let slot = self
                .documents
                .slot(session.target.clone(), reference.clone())?;
            let mut slot = slot.lock_owned().await;
            self.inspect(session, move |source, cancel| {
                documents::read(&mut slot, &source, &reference, query, cancel)
            })
            .await?
            .ok_or(AppError::NotFound)
        })
    }

    fn search(
        &self,
        session: HistorySession,
        query: HistorySearch,
    ) -> AppFuture<'_, Option<SearchPage>> {
        Box::pin(async move {
            let _pending = self
                .pending_searches
                .try_acquire()
                .map_err(|_| AppError::Busy)?;
            let _slot = self
                .searches
                .acquire()
                .await
                .map_err(|_| AppError::Unavailable)?;
            let owner = session.target.clone();
            let large = self.large_records.clone();
            let index = self.search_index.clone();
            let documents = self.documents.clone();
            self.inspect(session, move |source, stop| {
                let mut large = large.try_lock().map_err(|_| AppError::Busy)?;
                search::read(
                    &source,
                    &owner,
                    query,
                    stop,
                    &mut large,
                    &documents,
                    index.as_deref(),
                )
            })
            .await
        })
    }

    fn confirm_search_hit(
        &self,
        session: HistorySession,
        observation: SourceObservation,
        reference: FileContentRef,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.inspect(session, move |source, _| {
                search::confirm(&source, &observation, &reference)
            })
            .await?
            .ok_or(AppError::CursorExpired)
        })
    }
}

pub(crate) async fn observe_async_questions(
    session: HistorySession,
    started_at_ms: i64,
    known: Vec<ObservedAsyncQuestion>,
) -> Result<Vec<ObservedAsyncQuestion>, AppError> {
    let stopped = Cancel(Arc::new(AtomicBool::new(false)));
    let cancel = stopped.0.clone();
    tokio::task::spawn_blocking(move || match locator::locate(&session, &cancel)? {
        Some(source) => questions::observe(&source, started_at_ms, &known, &cancel),
        None => Ok(Vec::new()),
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

pub(crate) async fn locate_native_question_forms(
    session: HistorySession,
    path: PathBuf,
    forms: &mut [QuestionForm],
) {
    let wanted = forms
        .iter()
        .map(|form| (form.turn.clone(), form.item.clone()))
        .collect::<Vec<_>>();
    if wanted.is_empty()
        || session
            .source
            .as_ref()
            .is_some_and(|source| source.as_path() != path)
    {
        return;
    }
    let stopped = Cancel(Arc::new(AtomicBool::new(false)));
    let cancel = stopped.0.clone();
    let lookup = tokio::task::spawn_blocking(move || {
        let source = locator::verify(&session.binding, session.parent.as_ref(), &path, &cancel)?
            .ok_or(AppError::NotFound)?;
        questions::native_locations(&source, &wanted, &cancel)
    });
    if let Ok(Ok(mut found)) = lookup.await {
        for form in forms {
            form.locator = found.remove(&(form.turn.clone(), form.item.clone()));
        }
    }
}

pub(crate) async fn verify_pending_async_question(
    mut session: HistorySession,
    path: std::path::PathBuf,
    mut evidence: ObservedAsyncQuestion,
) -> Result<bool, AppError> {
    if evidence.state != reviewq_app::hosts::QuestionStatus::Submitting {
        return Err(AppError::PreconditionFailed);
    }
    let stopped = Cancel(Arc::new(AtomicBool::new(false)));
    let cancel = stopped.0.clone();
    let source = reviewq_domain::session::AbsolutePath::from_canonical(path)
        .map_err(|_| AppError::Conflict)?;
    if session.source.as_ref().is_some_and(|path| path != &source) {
        return Err(AppError::Conflict);
    }
    session.source = Some(source);
    evidence.state = reviewq_app::hosts::QuestionStatus::Pending;
    tokio::task::spawn_blocking(move || {
        let source = locator::locate(&session, &cancel)?.ok_or(AppError::Unavailable)?;
        let form = questions::prove(&source, &evidence, &cancel)?;
        match form.state {
            reviewq_app::hosts::QuestionStatus::Pending
                if !form.questions.iter().any(|q| q.secret) =>
            {
                Ok(true)
            }
            reviewq_app::hosts::QuestionStatus::Answered => Ok(false),
            _ => Err(AppError::Forbidden),
        }
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}
