use std::collections::{BTreeMap, BTreeSet, HashSet};

use reviewq_domain::identity::{Revision, TicketKey};
use reviewq_domain::settings::{BoardType, WorkspaceScope};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::sources::{ScopedSprint, TicketSourceReader};

use super::SprintPair;

pub struct LoadedSprint {
    pub sprint: ScopedSprint,
    pub tickets: BTreeSet<TicketKey>,
}

pub trait SprintStore: Send + Sync {
    fn unloaded_sprints(
        &self,
        actor: Actor,
        settings: Revision,
        pairs: Vec<SprintPair>,
    ) -> AppFuture<'_, Snapshot<Vec<SprintPair>>>;
    fn publish_sprints(
        &self,
        actor: Actor,
        settings: Revision,
        loaded: Vec<LoadedSprint>,
    ) -> AppFuture<'_, Snapshot<Vec<SprintPair>>>;
}

pub async fn collect_sprint_pairs(
    source: &dyn TicketSourceReader,
    scope: &WorkspaceScope,
    pairs: Vec<SprintPair>,
) -> Result<Vec<LoadedSprint>, AppError> {
    let mut requested = BTreeMap::<u64, BTreeSet<u64>>::new();
    for pair in pairs {
        requested.entry(pair.board).or_default().insert(pair.sprint);
    }
    let mut loaded = Vec::new();
    let mut total = 0;
    for (board, mut ids) in requested {
        let binding = scope
            .boards
            .iter()
            .find(|item| {
                item.selection.board_id == board
                    && item.selection.enabled
                    && item.board_type == BoardType::Scrum
            })
            .ok_or(AppError::NotFound)?;
        let mut offset = 0;
        for _ in 0..2000 {
            let page = source.sprints(board, offset, true).await?;
            for sprint in page.items {
                if sprint.board != board {
                    return Err(AppError::Conflict);
                }
                if !ids.remove(&sprint.id) {
                    continue;
                }
                let mut keys = BTreeSet::new();
                let mut seen = HashSet::new();
                let mut next = None;
                for _ in 0..2000 {
                    let page = source
                        .issues(binding.clone(), Some(sprint.id), next, 100)
                        .await?;
                    for issue in page.items {
                        if !keys.insert(issue.key) {
                            return Err(AppError::Conflict);
                        }
                        total += 1;
                        if total > 16_384 {
                            return Err(AppError::PayloadTooLarge);
                        }
                    }
                    let Some(token) = page.continuation else {
                        next = None;
                        break;
                    };
                    if !seen.insert(token.clone()) {
                        return Err(AppError::Conflict);
                    }
                    next = Some(token);
                }
                if next.is_some() {
                    return Err(AppError::PayloadTooLarge);
                }
                loaded.push(LoadedSprint {
                    sprint,
                    tickets: keys,
                });
            }
            if ids.is_empty() {
                break;
            }
            let Some(next) = page.next else {
                return Err(AppError::NotFound);
            };
            if next <= offset {
                return Err(AppError::Conflict);
            }
            offset = next;
        }
        if !ids.is_empty() {
            return Err(AppError::PayloadTooLarge);
        }
    }
    Ok(loaded)
}
