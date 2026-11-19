//! Discover direct descendants through the exact registered Codex parent.
use reviewq_app::access::AppError;
use reviewq_app::history::{HistoryParent, HistorySession};
use reviewq_app::notifications::{DiscoveredChildren, NativeChildCursor};
use serde::Deserialize;
use serde_json::json;

use super::Codex;
use super::session::Thread;

const PAGE_LIMIT: usize = 8;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadPage {
    data: Vec<Thread>,
    next_cursor: Option<String>,
}

impl Codex {
    pub(super) async fn children(
        &self,
        owner: &HistorySession,
        position: Option<&NativeChildCursor>,
    ) -> Result<DiscoveredChildren, AppError> {
        let parent = owner.binding.identity().native_id();
        let mut archived = position.is_some_and(|value| value.archived);
        let mut cursor = position.map(|value| value.cursor.as_str());
        let mut children = Vec::new();
        let mut next = None;
        let relation = HistoryParent {
            session_key: owner.target.session_key().clone(),
            native_id: parent.clone(),
            root_native_id: None,
            source: owner.source.clone(),
        };
        loop {
            let page: ThreadPage = self
                .connection
                .read(
                    "thread/list",
                    json!({
                        "parentThreadId":parent.as_str(),
                        "sourceKinds":["subAgentThreadSpawn"],
                        "useStateDbOnly":true,
                        "archived":archived,
                        "limit":PAGE_LIMIT,
                        "cursor":cursor,
                    }),
                )
                .await?;
            if page.data.len() > PAGE_LIMIT {
                return Err(AppError::PayloadTooLarge);
            }
            for thread in page.data {
                if thread.parent_thread_id.as_deref() != Some(parent.as_str()) {
                    return Err(AppError::Conflict);
                }
                if let Ok(id) = thread.id.parse()
                    && &id != parent
                    && let Ok(proof) = self.prove_thread(thread, id, Some(relation.clone())).await
                {
                    children.push(proof);
                }
            }
            if let Some(value) = page.next_cursor {
                if value.is_empty() || value.len() > 4096 || Some(value.as_str()) == cursor {
                    return Err(AppError::Unavailable);
                }
                next = Some(NativeChildCursor {
                    cursor: value,
                    archived,
                });
                break;
            }
            if archived {
                break;
            }
            archived = true;
            cursor = None;
        }
        Ok(DiscoveredChildren { children, next })
    }
}
