//! Visible native history for one verified lead.

use std::collections::BTreeMap;

use reviewq_domain::identity::{NativeInputId, NativeSessionId, NativeTurnId, RunId, SessionKey};
use reviewq_domain::session::{AbsolutePath, NativeBinding, SessionTarget};

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;
use crate::hosts::{ObservedAsyncQuestion, QuestionForm};

pub mod search;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistorySession {
    pub target: SessionTarget,
    pub binding: NativeBinding,
    pub source: Option<AbsolutePath>,
    pub parent: Option<HistoryParent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryParent {
    pub session_key: SessionKey,
    pub native_id: NativeSessionId,
    pub root_native_id: Option<NativeSessionId>,
    pub source: Option<AbsolutePath>,
}

pub trait HistoryStore: Send + Sync {
    fn prompt_overview_checkpoint(
        &self,
        source: HistorySession,
        start_offset: u64,
    ) -> AppFuture<'_, Option<PromptOverviewCheckpoint>>;
    fn remember_prompt_overview(
        &self,
        source: HistorySession,
        expected: Option<SourceAnchor>,
        checkpoint: PromptOverviewCheckpoint,
    ) -> AppFuture<'_, ()>;

    fn verified_history_turns(
        &self,
        actor: Actor,
        source: HistorySession,
        entries: Vec<NativeInputId>,
    ) -> AppFuture<'_, BTreeMap<NativeInputId, NativeTurnId>>;

    fn history_session(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
    ) -> AppFuture<'_, Snapshot<HistorySession>>;
}

pub trait NativeHistoryReader: Send + Sync {
    fn page(
        &self,
        session: HistorySession,
        position: NativePosition,
        limit: u16,
    ) -> AppFuture<'_, Option<NativeHistoryPage>>;

    fn around(
        &self,
        session: HistorySession,
        id: NativeInputId,
        anchor: Option<SourceAnchor>,
        limit: u16,
    ) -> AppFuture<'_, Option<NativeHistoryPage>>;

    fn navigation(
        &self,
        session: HistorySession,
        query: NavigationQuery,
    ) -> AppFuture<'_, Option<NativeNavigation>>;

    fn extend_overview(
        &self,
        session: HistorySession,
        checkpoint: PromptOverviewCheckpoint,
        pinned_start: Option<FilePosition>,
    ) -> AppFuture<'_, Option<PromptOverviewCheckpoint>>;

    fn plan(&self, session: HistorySession) -> AppFuture<'_, Option<NativePlan>>;

    fn final_response(&self, session: HistorySession)
    -> AppFuture<'_, Option<NativeFinalResponse>>;

    fn async_questions(
        &self,
        session: HistorySession,
        known: Vec<ObservedAsyncQuestion>,
        request: Option<String>,
    ) -> AppFuture<'_, Option<Vec<QuestionForm>>>;

    fn prove_async_question(
        &self,
        session: HistorySession,
        evidence: ObservedAsyncQuestion,
    ) -> AppFuture<'_, Option<QuestionForm>>;

    fn watch(
        &self,
        _session: HistorySession,
    ) -> AppFuture<'_, Option<Box<dyn NativeHistoryWatch>>> {
        Box::pin(async { Ok(None) })
    }

    fn head(
        &self,
        session: HistorySession,
        previous: Option<SourceAnchor>,
    ) -> AppFuture<'_, Option<NativeHead>>;

    fn parts(
        &self,
        session: HistorySession,
        reference: FileContentRef,
        first: u16,
    ) -> AppFuture<'_, NativeParts>;

    fn content(
        &self,
        session: HistorySession,
        reference: FileContentRef,
        offset: u64,
        limit: u32,
    ) -> AppFuture<'_, NativeContent>;

    fn search(
        &self,
        session: HistorySession,
        query: search::HistorySearch,
    ) -> AppFuture<'_, Option<search::SearchPage>>;

    fn confirm_search_hit(
        &self,
        session: HistorySession,
        observation: search::SourceObservation,
        reference: FileContentRef,
    ) -> AppFuture<'_, ()>;
}

/// Read-only live evidence for a selected native source. Durable pages remain authoritative.
#[derive(Clone)]
pub enum NativeLiveSignal {
    AssistantDelta {
        turn: NativeTurnId,
        entry: NativeInputId,
        offset: u64,
        text: String,
    },
    Completed,
    Reset,
}

pub trait NativeHistoryWatch: Send {
    fn changed(&mut self) -> AppFuture<'_, bool>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativePosition {
    pub cursor: Option<String>,
    pub ascending: bool,
    pub skip_turn: Option<NativeTurnId>,
    pub within: Option<TurnWindow>,
    /// Complete record boundary from the verified native file descriptor.
    pub file: Option<FilePosition>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FilePosition {
    pub offset: u64,
    pub high_water: u64,
    pub witness: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAnchor {
    pub generation: String,
    pub file: FilePosition,
}

#[derive(Clone, Debug)]
pub struct NativeHead {
    pub path: AbsolutePath,
    pub anchor: SourceAnchor,
    pub reset: bool,
}

#[derive(Clone, Debug)]
pub enum HistoryBookmark {
    File(FilePosition),
    Rpc(NativePosition),
}

impl HistoryBookmark {
    pub fn into_file(self) -> Result<FilePosition, crate::access::AppError> {
        match self {
            Self::File(file) => Ok(file),
            Self::Rpc(_) => Err(crate::access::AppError::CursorExpired),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PromptPosition {
    pub next_ordinal: u64,
    pub bookmark: HistoryBookmark,
}

#[derive(Clone)]
pub enum NavigationQuery {
    Overview {
        groups: u16,
        start: Option<(PromptPosition, String)>,
    },
    Previews {
        start: PromptPosition,
        first: u64,
        limit: u16,
        generation: String,
    },
}

#[derive(Clone)]
pub struct PromptGroup {
    pub first: u64,
    pub last: u64,
    pub start: PromptPosition,
}

pub struct PromptPreview {
    pub ordinal: u64,
    pub entry: NativeInputId,
    pub at_ms: Option<i64>,
    pub text: String,
    pub anchor: HistoryBookmark,
    pub after: PromptPosition,
}

pub enum NativeNavigation {
    Overview {
        generation: String,
        total: u64,
        complete: bool,
        groups: Vec<PromptGroup>,
        next: Option<PromptPosition>,
        source: Option<SourceAnchor>,
    },
    Previews {
        generation: String,
        complete: bool,
        items: Vec<PromptPreview>,
        next: Option<PromptPosition>,
    },
}

/// A bounded navigation window containing source witnesses and prompt ordinals.
#[derive(Clone)]
pub struct PromptOverviewCheckpoint {
    pub start_offset: u64,
    pub first_ordinal: u64,
    pub anchor: SourceAnchor,
    pub total: u64,
    pub groups: Vec<PromptGroup>,
    pub next: Option<PromptPosition>,
}

impl PromptOverviewCheckpoint {
    pub fn navigation(self) -> NativeNavigation {
        NativeNavigation::Overview {
            generation: self.anchor.generation.clone(),
            total: self.total,
            complete: self.next.is_none(),
            groups: self.groups,
            next: self.next,
            source: Some(self.anchor),
        }
    }
}

pub struct NativePlan {
    pub generation: String,
    pub entry: Option<(NativeInputId, VisiblePart)>,
    pub tasks: Option<Vec<NativeTask>>,
}

pub struct NativeFinalResponse {
    pub generation: String,
    pub entry: Option<VisibleEntry>,
    pub failure: Option<NativeTerminalFailure>,
}

pub struct NativeTerminalFailure {
    pub turn: NativeTurnId,
    pub completed_at_ms: i64,
}

pub struct NativeTask {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
}

pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Eq, PartialEq)]
pub struct NativeRpcStamp {
    pub generation: String,
    pub tail: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FileContentRef {
    pub generation: String,
    pub entry: NativeInputId,
    pub part: Option<u16>,
    pub position: FilePosition,
}

#[derive(Clone, Debug)]
pub enum ContentReference {
    File(FileContentRef),
    Rpc(RpcContentRef),
}

#[derive(Clone, Debug)]
pub struct RpcContentRef {
    pub generation: String,
    pub turn: NativeTurnId,
    pub entry: NativeInputId,
    pub part: Option<u16>,
    pub hash: reviewq_domain::identity::ReportHash,
}

impl ContentReference {
    pub fn file(&self) -> Option<&FileContentRef> {
        match self {
            Self::File(reference) => Some(reference),
            Self::Rpc(_) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NativeContent {
    pub generation: String,
    pub text: String,
    pub next_offset: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnWindow {
    pub turn: NativeTurnId,
    pub forward: bool,
    pub item_cursor: Option<String>,
    pub item_anchor: Option<NativeInputId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisibleKind {
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolState {
    Running,
    Completed,
    Failed,
    Unknown,
}

#[derive(Clone, Debug)]
pub enum VisiblePartKind {
    Text(VisibleKind),
    ToolCall {
        call: NativeInputId,
        name: String,
        label: &'static str,
        state: ToolState,
    },
    ToolResult {
        call: NativeInputId,
        state: ToolState,
    },
    Plan,
    Assignment,
}

pub struct NativeParts {
    pub parts: Vec<VisiblePart>,
    pub next: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct VisiblePart {
    pub native_index: u16,
    pub kind: VisiblePartKind,
    pub text: String,
    pub content: Option<ContentReference>,
}

#[derive(Clone, Debug)]
pub struct VisibleEntry {
    pub id: NativeInputId,
    pub turn: Option<NativeTurnId>,
    pub at_ms: Option<i64>,
    pub kind: VisibleKind,
    pub text: String,
    pub content: Option<ContentReference>,
    pub text_bytes: Option<u64>,
    pub parts: Vec<VisiblePart>,
    pub next_part: Option<u16>,
}

#[derive(Debug)]
pub struct NativeHistoryPage {
    pub generation: String,
    pub entries: Vec<VisibleEntry>,
    pub older: Option<NativePosition>,
    pub newer: Option<NativePosition>,
    pub within: Option<(NativePosition, NativeTurnId)>,
}

impl NativeHistoryPage {
    pub fn newer_after_trim(&self, end: usize) -> Option<NativePosition> {
        if end == self.entries.len() {
            return self.newer.clone();
        }
        if let Some(record) = self
            .entries
            .get(end)
            .and_then(|entry| entry.content.as_ref())
            .and_then(ContentReference::file)
        {
            return Some(NativePosition {
                file: Some(record.position.clone()),
                ascending: true,
                ..NativePosition::default()
            });
        }
        let (position, turn) = self.within.as_ref()?;
        let anchor = self.entries.get(end.checked_sub(1)?)?.id.clone();
        let mut position = position.clone();
        position.within = Some(TurnWindow {
            turn: turn.clone(),
            forward: true,
            item_cursor: None,
            item_anchor: Some(anchor),
        });
        Some(position)
    }

    pub fn older_after_trim(&self, count: usize) -> Option<NativePosition> {
        if count == 0 {
            return self.older.clone();
        }
        if let Some(record) = self
            .entries
            .get(count)
            .and_then(|entry| entry.content.as_ref())
            .and_then(ContentReference::file)
        {
            return Some(NativePosition {
                file: Some(record.position.clone()),
                ..NativePosition::default()
            });
        }
        let (position, turn) = self.within.as_ref()?;
        let anchor = self.entries.get(count)?.id.clone();
        let mut position = position.clone();
        position.within = Some(TurnWindow {
            turn: turn.clone(),
            forward: false,
            item_cursor: None,
            item_anchor: Some(anchor),
        });
        Some(position)
    }
}
