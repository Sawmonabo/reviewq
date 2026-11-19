use std::collections::HashMap;
use std::sync::Mutex;

use reviewq_app::access::AppError;
use reviewq_app::history::{HistorySession, NativeLiveSignal, SourceAnchor};
use reviewq_domain::identity::{NativeInputId, NativeTurnId};
use reviewq_domain::session::{AbsolutePath, HostProvider};
use serde_json::Value;
use tokio::sync::broadcast;

use super::Codex;

const MAX_DELTA_BYTES: usize = 8 * 1024;
const PENDING_DELTAS: usize = 4;

#[derive(Default)]
pub(super) struct Live(Mutex<HashMap<String, Source>>);

struct Source {
    generation: String,
    send: broadcast::Sender<NativeLiveSignal>,
    item: Option<ActiveItem>,
}

struct ActiveItem {
    turn: NativeTurnId,
    entry: NativeInputId,
    offset: u64,
}

impl Codex {
    pub(super) async fn open_history_signals(
        &self,
        source: HistorySession,
        file: Option<(AbsolutePath, SourceAnchor)>,
        generation: String,
    ) -> Result<Option<broadcast::Receiver<NativeLiveSignal>>, AppError> {
        let identity = source.binding.identity();
        if identity.provider() != HostProvider::Codex || identity.profile() != &self.profile {
            return Ok(None);
        }
        let thread = self.bound_history_thread(&source).await?;
        if let Some((path, anchor)) = file {
            if thread.path.as_deref() != Some(path.as_path()) || anchor.generation != generation {
                return Err(AppError::Conflict);
            }
            crate::history::verify_codex_live_source(source, path, anchor).await?;
        } else if self.history_generation(&thread).await? != generation {
            return Err(AppError::CursorExpired);
        }
        let mut live = self
            .connection
            .live
            .0
            .lock()
            .map_err(|_| AppError::Internal)?;
        live.retain(|_, source| source.send.receiver_count() > 0);
        if live
            .get(&thread.id)
            .is_some_and(|source| source.generation != generation)
        {
            live.remove(&thread.id);
        }
        let source = live.entry(thread.id).or_insert_with(|| Source {
            generation,
            send: broadcast::channel(PENDING_DELTAS).0,
            item: None,
        });
        Ok(Some(source.send.subscribe()))
    }
}

impl Live {
    pub(super) fn active(&self) -> bool {
        let Ok(mut sources) = self.0.lock() else {
            return false;
        };
        sources.retain(|_, source| source.send.receiver_count() > 0);
        !sources.is_empty()
    }

    pub(super) fn disconnect(&self) {
        if let Ok(mut sources) = self.0.lock() {
            for source in sources.values_mut() {
                source.reset();
            }
            // Closing the watch lets its scoped owner reverify the daemon connection.
            sources.clear();
        }
    }

    pub(super) fn frame(&self, frame: &Value) {
        let Some(method @ ("item/started" | "item/completed" | "item/agentMessage/delta")) =
            frame.get("method").and_then(Value::as_str)
        else {
            return;
        };
        let Some(params) = frame.get("params") else {
            return;
        };
        let Some(thread) = params.get("threadId").and_then(Value::as_str) else {
            return;
        };
        let Ok(mut sources) = self.0.lock() else {
            return;
        };
        let Some(source) = sources.get_mut(thread) else {
            return;
        };
        if source.send.receiver_count() == 0 {
            sources.remove(thread);
            return;
        }
        let Some(turn) = params.get("turnId").and_then(Value::as_str) else {
            return;
        };
        if method == "item/agentMessage/delta" {
            source.delta(turn, params);
            return;
        }
        let item = &params["item"];
        if item.get("type").and_then(Value::as_str) != Some("agentMessage") {
            return;
        }
        let Some(entry) = item.get("id").and_then(Value::as_str) else {
            return;
        };
        if method == "item/completed" {
            if source.matches(turn, entry) {
                source.item = None;
            }
            let _ = source.send.send(NativeLiveSignal::Completed);
            return;
        }
        if source.matches(turn, entry) {
            return;
        }
        source.reset();
        if item.get("text").and_then(Value::as_str) == Some("")
            && ["moderation", "delivery", "questions"]
                .into_iter()
                .all(|field| {
                    item.get(field).is_none_or(|value| {
                        value.is_null() || value.as_array().is_some_and(Vec::is_empty)
                    })
                })
            && let (Ok(turn), Ok(entry)) = (turn.parse(), entry.parse())
        {
            source.item = Some(ActiveItem {
                turn,
                entry,
                offset: 0,
            });
        }
    }
}

impl Source {
    fn matches(&self, turn: &str, entry: &str) -> bool {
        self.item
            .as_ref()
            .is_some_and(|item| item.turn.as_str() == turn && item.entry.as_str() == entry)
    }

    fn reset(&mut self) {
        if self.item.take().is_some() {
            let _ = self.send.send(NativeLiveSignal::Reset);
        }
    }

    fn delta(&mut self, turn: &str, params: &Value) {
        let Some(active) = &mut self.item else {
            return;
        };
        if active.turn.as_str() != turn
            || params.get("itemId").and_then(Value::as_str) != Some(active.entry.as_str())
        {
            return;
        }
        let Some(text) = params.get("delta").and_then(Value::as_str) else {
            self.reset();
            return;
        };
        if text.is_empty() {
            return;
        }
        if text.len() > MAX_DELTA_BYTES {
            self.reset();
            return;
        }
        let offset = active.offset;
        active.offset += text.len() as u64;
        let _ = self.send.send(NativeLiveSignal::AssistantDelta {
            turn: active.turn.clone(),
            entry: active.entry.clone(),
            offset,
            text: text.to_owned(),
        });
    }
}
