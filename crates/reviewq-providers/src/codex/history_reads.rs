use reviewq_app::access::AppError;
use reviewq_app::history::{
    ContentReference, HistorySession, NativeContent, NativeHistoryPage, NativePlan, NativePosition,
    RpcContentRef, VisibleEntry, VisiblePartKind,
};
use reviewq_domain::identity::{NativeInputId, NativeTurnId, ReportHash};
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::Codex;
use super::history::{Item, visible};
use super::session::canonical_cwd;

impl Codex {
    pub(super) async fn item_page(
        &self,
        thread: &str,
        turn: &NativeTurnId,
        forward: bool,
        cursor: Option<&str>,
        anchor: Option<&NativeInputId>,
        limit: u16,
    ) -> Result<ItemPage, AppError> {
        if cursor.is_some() && anchor.is_some() {
            return Err(AppError::CursorExpired);
        }
        let cursor = cursor
            .map(|value| json!(value))
            .or_else(|| anchor.map(|item| json!({"type":"item","itemId":item.as_str()})));
        let page: ItemPage = self
            .connection
            .read(
                "thread/items/list",
                json!({
                    "threadId":thread,"turnId":turn.as_str(),"cursor":cursor,
                    "sortDirection":if forward {"asc"} else {"desc"},"limit":limit,
                }),
            )
            .await?;
        if page.data.len() > usize::from(limit)
            || page.data.iter().any(|item| item.turn_id != turn.as_str())
            || [&page.next_cursor, &page.backwards_cursor]
                .iter()
                .any(|cursor| {
                    cursor
                        .as_ref()
                        .is_some_and(|value| value.is_empty() || value.len() > 2048)
                })
        {
            return Err(AppError::Unavailable);
        }
        Ok(page)
    }

    pub(super) async fn read_item_content(
        &self,
        session: HistorySession,
        reference: RpcContentRef,
        offset: u64,
        limit: u32,
    ) -> Result<NativeContent, AppError> {
        let native = session.binding.identity();
        if native.provider() != HostProvider::Codex || native.profile() != &self.profile {
            return Err(AppError::NotFound);
        }
        if !(4..=65_536).contains(&limit) {
            return Err(AppError::InvalidRequest);
        }
        let thread = self.read_thread(native.native_id()).await?;
        if canonical_cwd(thread.cwd.clone()).await? != *session.binding.cwd() {
            return Err(AppError::Conflict);
        }
        let generation = self.history_generation(&thread).await?;
        if generation != reference.generation {
            return Err(AppError::CursorExpired);
        }
        let response:ItemResponse=self.connection.read("thread/items/read",json!({
            "threadId":thread.id,"turnId":reference.turn.as_str(),"itemIds":[reference.entry.as_str()],
        })).await?;
        let [item]: [ItemEntry; 1] = response
            .data
            .try_into()
            .map_err(|_| AppError::CursorExpired)?;
        if item.turn_id != reference.turn.as_str() {
            return Err(AppError::CursorExpired);
        }
        let entry = visible(
            item.item,
            &reference.turn,
            item.started_at_ms,
            item.completed_at_ms,
        )
        .transpose()?
        .filter(|entry| entry.id == reference.entry)
        .ok_or(AppError::CursorExpired)?;
        let text = match reference.part {
            Some(index) => {
                &entry
                    .parts
                    .get(usize::from(index))
                    .ok_or(AppError::CursorExpired)?
                    .text
            }
            None => &entry.text,
        };
        if digest(text) != reference.hash {
            return Err(AppError::CursorExpired);
        }
        let start = usize::try_from(offset).map_err(|_| AppError::InvalidRequest)?;
        if start > text.len() || !text.is_char_boundary(start) {
            return Err(AppError::InvalidRequest);
        }
        let end = text.floor_char_boundary(start.saturating_add(limit as usize).min(text.len()));
        Ok(NativeContent {
            generation,
            text: text[start..end].to_owned(),
            next_offset: (end < text.len()).then_some(end as u64),
        })
    }

    pub(super) async fn read_around_item(
        &self,
        session: HistorySession,
        id: NativeInputId,
        anchor: Option<(NativePosition, String)>,
        limit: u16,
    ) -> Result<NativeHistoryPage, AppError> {
        if let Some((position, generation)) = anchor {
            let mut page = self.read_history(session, position, 32).await?;
            let index = page
                .entries
                .iter()
                .position(|entry| entry.id == id)
                .filter(|_| page.generation == generation)
                .ok_or(AppError::CursorExpired)?;
            let first = index.saturating_sub(usize::from(limit / 2));
            let end = first
                .saturating_add(usize::from(limit))
                .min(page.entries.len());
            page.older = page.older_after_trim(first);
            page.newer = page.newer_after_trim(end);
            page.entries = page
                .entries
                .into_iter()
                .skip(first)
                .take(end - first)
                .collect();
            return Ok(page);
        }
        let mut position = NativePosition::default();
        for _ in 0..24 {
            let page = self
                .read_history(session.clone(), position.clone(), limit)
                .await?;
            if page.entries.iter().any(|entry| entry.id == id) {
                return Ok(page);
            }
            let Some(next) = page.older.filter(|next| next != &position) else {
                return Err(AppError::NotFound);
            };
            position = next;
        }
        Err(AppError::Unavailable)
    }

    pub(super) async fn read_history_stamp(
        &self,
        session: HistorySession,
    ) -> Result<reviewq_app::history::NativeRpcStamp, AppError> {
        let native = session.binding.identity();
        if native.provider() != HostProvider::Codex || native.profile() != &self.profile {
            return Err(AppError::NotFound);
        }
        let thread = self.read_thread(native.native_id()).await?;
        if canonical_cwd(thread.cwd.clone()).await? != *session.binding.cwd() {
            return Err(AppError::Conflict);
        }
        let generation = self.history_generation(&thread).await?;
        let page = self.turns(&thread.id, None, false, 1, false).await?;
        let Some(turn) = page.data.first() else {
            return Ok(reviewq_app::history::NativeRpcStamp {
                generation,
                tail: None,
            });
        };
        let id = turn.id.parse().map_err(|_| AppError::Unavailable)?;
        let latest = self
            .item_page(&thread.id, &id, false, None, None, 1)
            .await?;
        let visible = latest
            .data
            .into_iter()
            .find_map(|item| visible(item.item, &id, item.started_at_ms, item.completed_at_ms))
            .transpose()?;
        let tail = if let Some(item) = visible {
            let mut hash = Sha256::new();
            for field in [
                turn.id.as_str(),
                turn.status.as_str(),
                item.id.as_str(),
                item.text.as_str(),
            ] {
                hash.update((field.len() as u64).to_be_bytes());
                hash.update(field.as_bytes());
            }
            for part in item.parts {
                hash.update((part.text.len() as u64).to_be_bytes());
                hash.update(part.text.as_bytes());
            }
            ReportHash::from_sha256(hash.finalize().into()).to_string()
        } else {
            format!("{}:{}", turn.id, turn.status)
        };
        Ok(reviewq_app::history::NativeRpcStamp {
            generation,
            tail: Some(tail),
        })
    }

    pub(super) async fn read_current_plan(
        &self,
        session: HistorySession,
    ) -> Result<NativePlan, AppError> {
        let mut position = NativePosition::default();
        for _ in 0..24 {
            let page = self
                .read_history(session.clone(), position.clone(), 30)
                .await?;
            let plan = page.entries.into_iter().rev().find_map(|entry| {
                entry
                    .parts
                    .into_iter()
                    .rev()
                    .find(|part| matches!(part.kind, VisiblePartKind::Plan))
                    .map(|part| (entry.id, part))
            });
            if plan.is_some() || page.older.is_none() {
                return Ok(NativePlan {
                    generation: page.generation,
                    entry: plan,
                    tasks: None,
                });
            }
            position = page
                .older
                .filter(|next| next != &position)
                .ok_or(AppError::CursorExpired)?;
        }
        Err(AppError::Unavailable)
    }
}

pub(super) fn preview(mut entry: VisibleEntry, generation: &str) -> VisibleEntry {
    let Some(turn) = entry.turn.as_ref() else {
        return entry;
    };
    let entry_id = &entry.id;
    let reference = |body: &str, part| {
        ContentReference::Rpc(RpcContentRef {
            generation: generation.to_owned(),
            turn: turn.clone(),
            entry: entry_id.clone(),
            part,
            hash: digest(body),
        })
    };
    entry.text_bytes = Some(entry.text.len() as u64);
    if let Some((end, _)) = entry.text.char_indices().nth(512) {
        entry.content = Some(reference(&entry.text, None));
        entry.text.truncate(end);
    }
    for (index, part) in entry.parts.iter_mut().enumerate() {
        let cap = part.text.floor_char_boundary(part.text.len().min(256));
        if part.text.len() > cap {
            part.content = Some(reference(&part.text, Some(index as u16)));
            part.text.truncate(cap);
        }
    }
    entry
}

fn digest(value: &str) -> ReportHash {
    ReportHash::from_sha256(Sha256::digest(value.as_bytes()).into())
}

#[derive(Deserialize)]
struct ItemResponse {
    data: Vec<ItemEntry>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ItemEntry {
    turn_id: String,
    pub(super) item: Item,
    pub(super) started_at_ms: Option<i64>,
    pub(super) completed_at_ms: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ItemPage {
    pub(super) data: Vec<ItemEntry>,
    pub(super) next_cursor: Option<String>,
    pub(super) backwards_cursor: Option<String>,
}
