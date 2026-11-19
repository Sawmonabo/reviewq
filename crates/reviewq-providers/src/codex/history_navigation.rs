use reviewq_app::access::AppError;
use reviewq_app::history::{
    HistoryBookmark, HistorySession, NativeHistoryPage, NativeNavigation, NativePosition,
    NavigationQuery, PromptGroup, PromptPosition, PromptPreview, VisibleKind,
};

use super::Codex;

const PAGES: u16 = 16;
const ITEMS: u16 = 24;

impl Codex {
    pub(super) async fn read_prompt_navigation(
        &self,
        session: HistorySession,
        query: NavigationQuery,
    ) -> Result<NativeNavigation, AppError> {
        match query {
            NavigationQuery::Overview { groups, start } if (1..=64).contains(&groups) => {
                let (mut position, mut ordinal, mut generation) = match start {
                    Some((start, generation)) => {
                        let HistoryBookmark::Rpc(position) = start.bookmark else {
                            return Err(AppError::CursorExpired);
                        };
                        (position, start.next_ordinal, Some(generation))
                    }
                    None => (
                        NativePosition {
                            ascending: true,
                            ..NativePosition::default()
                        },
                        1,
                        None,
                    ),
                };
                let mut groups_found = Vec::new();
                let mut next = None;
                for _ in 0..groups.min(PAGES) {
                    let current = position.clone();
                    let page = self
                        .read_history(session.clone(), current.clone(), ITEMS)
                        .await?;
                    bind_generation(&mut generation, &page)?;
                    let first = ordinal;
                    ordinal += page
                        .entries
                        .iter()
                        .filter(|entry| entry.kind == VisibleKind::User)
                        .count() as u64;
                    if ordinal > first {
                        groups_found.push(PromptGroup {
                            first,
                            last: ordinal - 1,
                            start: bookmark(first, current.clone()),
                        });
                    }
                    next = progress(&current, &page)?;
                    let Some(newer) = next.as_ref() else {
                        break;
                    };
                    position = newer.clone();
                }
                Ok(NativeNavigation::Overview {
                    generation: generation.ok_or(AppError::Internal)?,
                    total: ordinal - 1,
                    complete: next.is_none(),
                    groups: groups_found,
                    next: next.map(|position| bookmark(ordinal, position)),
                })
            }
            NavigationQuery::Previews {
                start,
                first,
                limit,
                generation,
            } if (1..=50).contains(&limit) && start.next_ordinal <= first => {
                let HistoryBookmark::Rpc(mut position) = start.bookmark else {
                    return Err(AppError::CursorExpired);
                };
                let mut ordinal = start.next_ordinal;
                let mut generation = Some(generation);
                let mut items = Vec::new();
                let mut next = None;
                'pages: for _ in 0..PAGES {
                    let current = position.clone();
                    let page = self
                        .read_history(session.clone(), current.clone(), ITEMS)
                        .await?;
                    bind_generation(&mut generation, &page)?;
                    next = progress(&current, &page)?.map(|next| bookmark(ordinal, next));
                    for (index, entry) in page.entries.iter().enumerate() {
                        if entry.kind != VisibleKind::User {
                            continue;
                        }
                        let current_ordinal = ordinal;
                        ordinal += 1;
                        if current_ordinal < first {
                            continue;
                        }
                        let after = match page.newer_after_trim(index + 1) {
                            Some(position) => position,
                            None => {
                                let (base, turn) =
                                    page.within.as_ref().ok_or(AppError::Unavailable)?;
                                let mut position = base.clone();
                                position.within = Some(reviewq_app::history::TurnWindow {
                                    turn: turn.clone(),
                                    forward: true,
                                    item_cursor: None,
                                    item_anchor: Some(entry.id.clone()),
                                });
                                position
                            }
                        };
                        items.push(PromptPreview {
                            ordinal: current_ordinal,
                            entry: entry.id.clone(),
                            at_ms: entry.at_ms,
                            text: entry.text.chars().take(160).collect(),
                            anchor: HistoryBookmark::Rpc(current.clone()),
                            after: bookmark(ordinal, after.clone()),
                        });
                        if items.len() == usize::from(limit) {
                            next = page
                                .newer_after_trim(index + 1)
                                .map(|next| bookmark(ordinal, next));
                            break 'pages;
                        }
                    }
                    if let Some(bookmark) = next.as_mut() {
                        bookmark.next_ordinal = ordinal;
                        if let HistoryBookmark::Rpc(newer) = &bookmark.bookmark {
                            position = newer.clone();
                        }
                    } else {
                        break;
                    }
                }
                Ok(NativeNavigation::Previews {
                    generation: generation.ok_or(AppError::Internal)?,
                    complete: next.is_none(),
                    items,
                    next,
                })
            }
            _ => Err(AppError::InvalidRequest),
        }
    }
}

fn bookmark(next_ordinal: u64, position: NativePosition) -> PromptPosition {
    PromptPosition {
        next_ordinal,
        bookmark: HistoryBookmark::Rpc(position),
    }
}
fn bind_generation(current: &mut Option<String>, page: &NativeHistoryPage) -> Result<(), AppError> {
    if current
        .as_ref()
        .is_some_and(|prior| prior != &page.generation)
    {
        return Err(AppError::CursorExpired);
    }
    current.get_or_insert_with(|| page.generation.clone());
    Ok(())
}
fn progress(
    current: &NativePosition,
    page: &NativeHistoryPage,
) -> Result<Option<NativePosition>, AppError> {
    if page.newer.as_ref() == Some(current) {
        return Err(AppError::CursorExpired);
    }
    Ok(page.newer.clone())
}
