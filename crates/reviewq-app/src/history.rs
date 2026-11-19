//! Visible native history for one verified lead.

use std::collections::BTreeMap;

use reviewq_domain::identity::{NativeInputId, NativeSessionId, NativeTurnId, RunId, SessionKey};
use reviewq_domain::session::{AbsolutePath, NativeBinding, SessionTarget};

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;

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

    fn plan(&self, session: HistorySession) -> AppFuture<'_, Option<NativePlan>>;

    fn head(
        &self,
        session: HistorySession,
        previous: Option<SourceAnchor>,
    ) -> AppFuture<'_, Option<NativeHead>>;

    fn content(
        &self,
        session: HistorySession,
        reference: FileContentRef,
        offset: u64,
        limit: u32,
    ) -> AppFuture<'_, NativeContent>;
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

#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub anchor: SourceAnchor,
    pub reset: bool,
}

#[derive(Clone, Debug)]
pub struct PromptPosition {
    pub next_ordinal: u64,
    pub file: FilePosition,
}

pub enum NavigationQuery {
    Overview {
        groups: u16,
    },
    Previews {
        start: PromptPosition,
        first: u64,
        limit: u16,
        generation: String,
    },
}

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
    pub anchor: FilePosition,
    pub after: PromptPosition,
}

pub enum NativeNavigation {
    Overview {
        generation: String,
        total: u64,
        complete: bool,
        groups: Vec<PromptGroup>,
    },
    Previews {
        generation: String,
        complete: bool,
        items: Vec<PromptPreview>,
        next: Option<PromptPosition>,
    },
}

pub struct NativePlan {
    pub generation: String,
    pub entry: Option<(NativeInputId, VisiblePart)>,
}

#[derive(Clone, Debug)]
pub struct FileContentRef {
    pub generation: String,
    pub entry: NativeInputId,
    pub part: Option<u16>,
    pub position: FilePosition,
}

#[derive(Debug)]
pub struct NativeContent {
    pub generation: String,
    pub text: String,
    pub next_offset: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnWindow {
    pub turn: NativeTurnId,
    pub index: usize,
    pub forward: bool,
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

#[derive(Clone, Debug)]
pub struct VisiblePart {
    pub kind: VisiblePartKind,
    pub text: String,
    pub content: Option<FileContentRef>,
}

#[derive(Clone, Debug)]
pub struct VisibleEntry {
    pub id: NativeInputId,
    pub turn: Option<NativeTurnId>,
    pub at_ms: Option<i64>,
    pub kind: VisibleKind,
    pub text: String,
    pub content: Option<FileContentRef>,
    pub text_bytes: Option<u64>,
    pub parts: Vec<VisiblePart>,
}

#[derive(Debug)]
pub struct NativeHistoryPage {
    pub generation: String,
    pub entries: Vec<VisibleEntry>,
    pub older: Option<NativePosition>,
    pub newer: Option<NativePosition>,
    pub within: Option<(NativePosition, NativeTurnId, usize)>,
}

impl NativeHistoryPage {
    pub fn older_after_trim(&self, count: usize) -> Option<NativePosition> {
        if count == 0 {
            return self.older.clone();
        }
        if let Some(record) = self
            .entries
            .get(count)
            .and_then(|entry| entry.content.as_ref())
        {
            return Some(NativePosition {
                file: Some(record.position.clone()),
                ..NativePosition::default()
            });
        }
        let (position, turn, start) = self.within.as_ref()?;
        let mut position = position.clone();
        position.within = Some(TurnWindow {
            turn: turn.clone(),
            index: start + count,
            forward: false,
        });
        Some(position)
    }
}
