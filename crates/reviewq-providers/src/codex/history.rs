use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

use reviewq_app::access::AppError;
use reviewq_app::history::{
    HistorySession, NativeHistoryPage, NativePosition, TurnWindow, VisibleEntry, VisibleKind,
};
use reviewq_domain::identity::NativeTurnId;
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::Codex;
use super::session::Thread;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Turns {
    pub data: Vec<Turn>,
    pub(super) next_cursor: Option<String>,
    backwards_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Turn {
    pub id: String,
    pub status: String,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    pub items: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(super) enum Item {
    UserMessage {
        id: String,
        client_id: Option<String>,
        content: Vec<Content>,
    },
    AgentMessage {
        id: String,
        text: String,
        phase: Option<String>,
    },
    CommandExecution {
        id: String,
        command: String,
        source: Option<String>,
        status: String,
        exit_code: Option<i32>,
        aggregated_output: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(super) enum Content {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

impl Codex {
    pub(super) async fn turns(
        &self,
        thread: &str,
        cursor: Option<&str>,
        ascending: bool,
        limit: u16,
        full: bool,
    ) -> Result<Turns, AppError> {
        let sort = if ascending { "asc" } else { "desc" };
        let view = if full { "full" } else { "notLoaded" };
        let turns: Turns = self.connection.read(
            "thread/turns/list",
            json!({"threadId":thread,"cursor":cursor,"sortDirection":sort,"limit":limit,"itemsView":view}),
        ).await?;
        if turns.data.len() > usize::from(limit)
            || turns
                .next_cursor
                .as_ref()
                .is_some_and(|value| value.len() > 2048)
            || turns
                .backwards_cursor
                .as_ref()
                .is_some_and(|value| value.len() > 2048)
        {
            return Err(AppError::Unavailable);
        }
        Ok(turns)
    }

    pub(super) async fn read_history(
        &self,
        target: HistorySession,
        mut position: NativePosition,
        limit: u16,
    ) -> Result<NativeHistoryPage, AppError> {
        if !(1..=100).contains(&limit) {
            return Err(AppError::InvalidRequest);
        }
        let identity = target.binding.identity();
        if identity.profile() != &self.profile || identity.provider() != HostProvider::Codex {
            return Err(AppError::NotFound);
        }
        let thread = self.read_thread(identity.native_id()).await?;
        if position.file.is_some() {
            return Err(AppError::CursorExpired);
        }
        let generation = self.history_generation(&thread).await?;
        let from_tail = position == NativePosition::default();
        let mut turns = self
            .turns(
                &thread.id,
                position.cursor.as_deref(),
                position.ascending,
                1,
                true,
            )
            .await?;
        if let Some(skip) = position.skip_turn.take() {
            let turn = turns.data.first().ok_or(AppError::CursorExpired)?;
            if turn.id != skip.as_str() {
                return Err(AppError::CursorExpired);
            }
            let next = turns.next_cursor.take().ok_or(AppError::CursorExpired)?;
            position.cursor = Some(next);
            turns = self
                .turns(
                    &thread.id,
                    position.cursor.as_deref(),
                    position.ascending,
                    1,
                    true,
                )
                .await?;
        }
        let Some(turn) = turns.data.pop() else {
            return Ok(NativeHistoryPage {
                generation,
                entries: Vec::new(),
                older: None,
                newer: None,
                within: None,
            });
        };
        let id: NativeTurnId = turn.id.parse().map_err(|_| AppError::Unavailable)?;
        let (mut older, mut newer) = turn_neighbors(&position, &id, turns, from_tail);
        let started = turn
            .started_at
            .and_then(|seconds| seconds.checked_mul(1000));
        let completed = turn
            .completed_at
            .and_then(|seconds| seconds.checked_mul(1000));
        let entries = turn
            .items
            .into_iter()
            .filter_map(|item| visible(item, &id, started, completed))
            .collect::<Result<Vec<_>, AppError>>()?;
        let count = usize::from(limit);
        let (start, end) = match position.within.take() {
            Some(within) if within.turn == id && within.index <= entries.len() => {
                if within.forward {
                    (
                        within.index,
                        within.index.saturating_add(count).min(entries.len()),
                    )
                } else {
                    (within.index.saturating_sub(count), within.index)
                }
            }
            Some(_) => return Err(AppError::CursorExpired),
            None if position.ascending => (0, count.min(entries.len())),
            None => (entries.len().saturating_sub(count), entries.len()),
        };
        if start > 0 {
            older = Some(within(&position, &id, start, false));
        }
        if end < entries.len() {
            newer = Some(within(&position, &id, end, true));
        }
        Ok(NativeHistoryPage {
            generation,
            entries: entries.into_iter().skip(start).take(end - start).collect(),
            older,
            newer,
            within: Some((position, id, start)),
        })
    }

    pub(super) async fn history_generation(&self, thread: &Thread) -> Result<String, AppError> {
        let mut hash = Sha256::new();
        hash.update(self.profile.as_path().as_os_str().as_bytes());
        hash.update([0]);
        hash.update(thread.id.as_bytes());
        if let Some(path) = &thread.path
            && let Ok(canonical) = tokio::fs::canonicalize(path).await
        {
            if !canonical.starts_with(self.profile.as_path()) {
                return Err(AppError::Forbidden);
            }
            let metadata = tokio::fs::metadata(&canonical)
                .await
                .map_err(|_| AppError::Unavailable)?;
            if !metadata.is_file() {
                return Err(AppError::Unavailable);
            }
            hash.update(metadata.dev().to_be_bytes());
            hash.update(metadata.ino().to_be_bytes());
        }
        Ok(hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
}

fn turn_neighbors(
    position: &NativePosition,
    id: &NativeTurnId,
    turns: Turns,
    from_tail: bool,
) -> (Option<NativePosition>, Option<NativePosition>) {
    let next = turns.next_cursor.map(|cursor| NativePosition {
        cursor: Some(cursor),
        ascending: position.ascending,
        ..NativePosition::default()
    });
    let previous = if from_tail {
        None
    } else {
        turns.backwards_cursor.map(|cursor| NativePosition {
            cursor: Some(cursor),
            ascending: !position.ascending,
            skip_turn: Some(id.clone()),
            within: None,
            file: None,
        })
    };
    if position.ascending {
        (previous, next)
    } else {
        (next, previous)
    }
}

fn within(
    position: &NativePosition,
    turn: &NativeTurnId,
    index: usize,
    forward: bool,
) -> NativePosition {
    let mut result = position.clone();
    result.within = Some(TurnWindow {
        turn: turn.clone(),
        index,
        forward,
    });
    result
}

fn visible(
    item: Item,
    turn: &NativeTurnId,
    started: Option<i64>,
    completed: Option<i64>,
) -> Option<Result<VisibleEntry, AppError>> {
    let (id, text, kind, at_ms) = match item {
        Item::UserMessage { id, content, .. } => {
            let text = content
                .into_iter()
                .filter_map(|part| match part {
                    Content::Text { text } => Some(text),
                    Content::Other => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (id, text, VisibleKind::User, started)
        }
        Item::AgentMessage { id, text, .. } => (id, text, VisibleKind::Assistant, completed),
        Item::Other | Item::CommandExecution { .. } => return None,
    };
    Some(
        id.parse()
            .map(|id| VisibleEntry {
                id,
                turn: Some(turn.clone()),
                at_ms,
                kind,
                text,
                content: None,
                text_bytes: None,
            })
            .map_err(|_| AppError::Unavailable),
    )
}
