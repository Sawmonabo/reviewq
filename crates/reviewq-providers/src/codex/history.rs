use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

use reviewq_app::access::AppError;
use reviewq_app::history::{
    HistorySession, NativeHistoryPage, NativePosition, TurnWindow, VisibleEntry, VisibleKind,
    VisiblePart, VisiblePartKind,
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
    pub root_turn_id: Option<String>,
    pub status: String,
    pub(super) completed_at: Option<i64>,
    pub(super) error: Option<TurnError>,
    pub items: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TurnError {
    pub(super) codex_error_info: Option<serde_json::Value>,
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
        questions: Option<Vec<serde_json::Value>>,
    },
    CommandExecution {
        id: String,
        command: String,
        source: Option<String>,
        status: String,
        exit_code: Option<i32>,
        aggregated_output: Option<String>,
    },
    Plan {
        id: String,
        text: String,
    },
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        status: String,
        arguments: Option<serde_json::Value>,
        result: Option<serde_json::Value>,
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
        if super::session::canonical_cwd(thread.cwd.clone()).await? != *target.binding.cwd() {
            return Err(AppError::Conflict);
        }
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
                false,
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
                    false,
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
        let in_turn = position.within.take();
        if in_turn.as_ref().is_some_and(|window| window.turn != id) {
            return Err(AppError::CursorExpired);
        }
        let forward = in_turn
            .as_ref()
            .map_or(position.ascending, |window| window.forward);
        let mut items = self
            .item_page(
                &thread.id,
                &id,
                forward,
                in_turn
                    .as_ref()
                    .and_then(|window| window.item_cursor.as_deref()),
                in_turn
                    .as_ref()
                    .and_then(|window| window.item_anchor.as_ref()),
                limit.min(32),
            )
            .await?;
        let mut entries = items
            .data
            .into_iter()
            .filter_map(|item| {
                visible(item.item, &id, item.started_at_ms, item.completed_at_ms).map(|entry| {
                    entry.map(|entry| super::history_reads::preview(entry, &generation))
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        if !forward {
            entries.reverse();
        }
        let next = items
            .next_cursor
            .take()
            .map(|cursor| within(&position, &id, cursor, forward));
        let backwards = in_turn
            .and_then(|_| items.backwards_cursor.take())
            .map(|cursor| within(&position, &id, cursor, !forward));
        if forward {
            newer = next.or(newer);
            older = backwards.or(older);
        } else {
            older = next.or(older);
            newer = backwards.or(newer);
        }
        Ok(NativeHistoryPage {
            generation,
            entries,
            older,
            newer,
            within: Some((position, id)),
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
    item_cursor: String,
    forward: bool,
) -> NativePosition {
    let mut result = position.clone();
    result.within = Some(TurnWindow {
        turn: turn.clone(),
        forward,
        item_cursor: Some(item_cursor),
        item_anchor: None,
    });
    result
}

pub(super) fn visible(
    item: Item,
    turn: &NativeTurnId,
    started: Option<i64>,
    completed: Option<i64>,
) -> Option<Result<VisibleEntry, AppError>> {
    let mut parts = Vec::new();
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
        Item::Plan { id, text } => {
            parts.push(VisiblePart {
                native_index: 0,
                kind: VisiblePartKind::Plan,
                text,
                content: None,
            });
            (id, String::new(), VisibleKind::Assistant, completed)
        }
        Item::CommandExecution {
            id,
            command,
            status,
            aggregated_output,
            ..
        } => {
            parts = rpc_tool(&id, "CommandExecution", status, command, aggregated_output)?;
            (id, String::new(), VisibleKind::Assistant, completed)
        }
        Item::McpToolCall {
            id,
            server,
            tool,
            status,
            arguments,
            result,
        } => {
            if server.len() + tool.len() > 128
                || server.chars().chain(tool.chars()).any(char::is_control)
            {
                return None;
            }
            let name = format!("mcp__{server}__{tool}");
            let arguments = arguments.map(|value| value.to_string()).unwrap_or_default();
            parts = rpc_tool(
                &id,
                &name,
                status,
                arguments,
                result.map(|value| value.to_string()),
            )?;
            (id, String::new(), VisibleKind::Assistant, completed)
        }
        Item::Other => return None,
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
                parts,
                next_part: None,
            })
            .map_err(|_| AppError::Unavailable),
    )
}

fn rpc_tool(
    id: &str,
    name: &str,
    status: String,
    argument: String,
    result: Option<String>,
) -> Option<Vec<VisiblePart>> {
    let call: reviewq_domain::identity::NativeInputId = id.parse().ok()?;
    let state = crate::history::tools::state(Some(&status));
    let mut parts = vec![VisiblePart {
        native_index: 0,
        kind: VisiblePartKind::ToolCall {
            call: call.clone(),
            name: name.to_owned(),
            label: crate::history::tools::label(name),
            state,
        },
        text: argument,
        content: None,
    }];
    if let Some(text) = result.filter(|value| !value.is_empty()) {
        parts.push(VisiblePart {
            native_index: 1,
            kind: VisiblePartKind::ToolResult { call, state },
            text,
            content: None,
        });
    }
    Some(parts)
}
