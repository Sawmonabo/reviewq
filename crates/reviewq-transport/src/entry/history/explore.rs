use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::history::{NativeNavigation, NavigationQuery, PromptPosition, SourceAnchor};
use reviewq_domain::identity::{NativeInputId, RunId, SessionKey};
use reviewq_protocol::history::{
    HistoryAvailability, HistoryNavigation, HistoryPlan, HistoryPlanResponse, HistoryText,
    NavigationPage, PlanEntry, PromptGroup, PromptPreview,
};
use reviewq_protocol::{
    HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, RunIdDto, SafeCountDto,
    SessionKeyDto, UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use super::{Entry, Point, ServiceEndpoint, visible_part};
use crate::entry::error::{ApiError, ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{parse, success};

const OVERVIEW: &[u8] = b"reviewq-native-prompt-window-v1";
const PREVIEWS: &[u8] = b"reviewq-native-prompt-page-v1";
const AROUND: &[u8] = b"reviewq-native-prompt-anchor-v1";
const MAX_ENVELOPE: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::entry) struct NavRequest {
    cursor: Option<String>,
    overview_cursor: Option<String>,
    max_groups: Option<u16>,
    limit: Option<u16>,
}

#[derive(Serialize, Deserialize)]
struct OverviewCursor {
    run: String,
    session: String,
    generation: String,
    groups: u16,
    next_ordinal: u64,
    point: Point,
}

#[derive(Serialize, Deserialize)]
struct PromptCursor {
    run: String,
    session: String,
    generation: String,
    next_ordinal: u64,
    wanted: u64,
    point: Point,
}

#[derive(Serialize, Deserialize)]
struct EntryLocator {
    run: String,
    session: String,
    generation: String,
    entry: String,
    point: Point,
}

pub(super) fn open_anchor(
    endpoint: &ServiceEndpoint,
    run: &RunId,
    session: &SessionKey,
    entry: &NativeInputId,
    token: &str,
) -> Result<SourceAnchor, AppError> {
    let locator: EntryLocator = endpoint.decode_cursor(AROUND, token)?;
    if locator.run != run.as_str()
        || locator.session != session.as_str()
        || locator.entry != entry.as_str()
    {
        return Err(AppError::CursorExpired);
    }
    Ok(SourceAnchor {
        generation: locator.generation,
        file: locator.point.into(),
    })
}

pub(in crate::entry) async fn navigation(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    Query(query): Query<NavRequest>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let run: RunId = parse(&run)?;
    let session: SessionKey = parse(&session)?;
    let source = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let endpoint = &entry.auth.endpoint;
    let mut groups = query.max_groups.unwrap_or(32);
    let nav = if let Some(token) = query.overview_cursor {
        if query.cursor.is_some() || query.max_groups.is_some() || query.limit.is_some() {
            return Err(AppError::InvalidRequest.into());
        }
        let cursor: OverviewCursor = endpoint.decode_cursor(OVERVIEW, &token)?;
        if cursor.run != run.as_str()
            || cursor.session != session.as_str()
            || !(1..=64).contains(&cursor.groups)
        {
            return Err(AppError::CursorExpired.into());
        }
        groups = cursor.groups;
        NavigationQuery::Overview {
            groups,
            start: Some((
                PromptPosition {
                    next_ordinal: cursor.next_ordinal,
                    file: cursor.point.into(),
                },
                cursor.generation,
            )),
        }
    } else if let Some(token) = query.cursor {
        if query.max_groups.is_some() || !(1..=50).contains(&query.limit.unwrap_or(30)) {
            return Err(AppError::InvalidRequest.into());
        }
        let cursor: PromptCursor = endpoint.decode_cursor(PREVIEWS, &token)?;
        if cursor.run != run.as_str() || cursor.session != session.as_str() {
            return Err(AppError::CursorExpired.into());
        }
        NavigationQuery::Previews {
            start: PromptPosition {
                next_ordinal: cursor.next_ordinal,
                file: cursor.point.into(),
            },
            first: cursor.wanted,
            limit: query.limit.unwrap_or(30),
            generation: cursor.generation,
        }
    } else {
        if query.limit.is_some() || !(1..=64).contains(&groups) {
            return Err(AppError::InvalidRequest.into());
        }
        NavigationQuery::Overview {
            groups,
            start: None,
        }
    };
    let (result, reset) = match entry
        .file_history
        .navigation(source.value.clone(), nav)
        .await
    {
        Ok(value) => (value, false),
        Err(AppError::CursorExpired) => (
            entry
                .file_history
                .navigation(
                    source.value.clone(),
                    NavigationQuery::Overview {
                        groups,
                        start: None,
                    },
                )
                .await?,
            true,
        ),
        Err(error) => return Err(error.into()),
    };
    let result = result.ok_or(AppError::Unavailable)?;
    if entry
        .store
        .history_session(actor, run.clone(), session.clone())
        .await?
        .value
        != source.value
    {
        return Err(AppError::CursorExpired.into());
    }
    let make_cursor = |generation: &str, wanted, position: &PromptPosition| {
        let value = PromptCursor {
            run: run.to_string(),
            session: session.to_string(),
            generation: generation.to_owned(),
            next_ordinal: position.next_ordinal,
            wanted,
            point: position.file.clone().into(),
        };
        HistoryCursorDto::new(endpoint.encode_cursor(PREVIEWS, &value)?).ok_or(AppError::Internal)
    };
    let encode =
        |generation: &str, complete: bool, page: NavigationPage| -> Result<Vec<u8>, AppError> {
            let response = success(
                HistoryNavigation {
                    run_id: RunIdDto::new(run.as_str()).ok_or(AppError::Internal)?,
                    session_key: SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)?,
                    generation: HistoryGenerationDto::new(generation).ok_or(AppError::Internal)?,
                    availability: HistoryAvailability::Available,
                    reset,
                    complete,
                    page,
                },
                source.cursor,
            )?;
            serde_json::to_vec(&response).map_err(|_| AppError::Internal)
        };
    let bytes = match result {
        NativeNavigation::Overview {
            generation,
            total,
            complete,
            groups: windows,
            next,
        } => {
            let next_window_cursor = next
                .map(|position| {
                    let cursor = OverviewCursor {
                        run: run.to_string(),
                        session: session.to_string(),
                        generation: generation.clone(),
                        groups,
                        next_ordinal: position.next_ordinal,
                        point: position.file.into(),
                    };
                    HistoryCursorDto::new(endpoint.encode_cursor(OVERVIEW, &cursor)?)
                        .ok_or(AppError::Internal)
                })
                .transpose()?;
            let groups = windows
                .into_iter()
                .map(|group| {
                    Ok(PromptGroup {
                        first: valid_count(group.first)?,
                        last: valid_count(group.last)?,
                        cursor: make_cursor(&generation, group.first, &group.start)?,
                    })
                })
                .collect::<Result<_, AppError>>()?;
            encode(
                &generation,
                complete,
                NavigationPage::Overview {
                    total: valid_count(total)?,
                    groups,
                    next_window_cursor,
                },
            )?
        }
        NativeNavigation::Previews {
            generation,
            complete,
            items,
            next,
        } => {
            let mut count = items.len();
            loop {
                let views = items
                    .iter()
                    .take(count)
                    .map(|item| {
                        let locator = EntryLocator {
                            run: run.to_string(),
                            session: session.to_string(),
                            generation: generation.clone(),
                            entry: item.entry.to_string(),
                            point: item.anchor.clone().into(),
                        };
                        Ok(PromptPreview {
                            ordinal: valid_count(item.ordinal)?,
                            entry_id: HistoryEntryIdDto::new(item.entry.as_str())
                                .ok_or(AppError::Internal)?,
                            timestamp: item.at_ms.and_then(UtcTimestampDto::from_unix_milliseconds),
                            text: HistoryText::new(item.text.as_str()).ok_or(AppError::Internal)?,
                            locator: HistoryCursorDto::new(
                                endpoint.encode_cursor(AROUND, &locator)?,
                            )
                            .ok_or(AppError::Internal)?,
                        })
                    })
                    .collect::<Result<_, AppError>>()?;
                let continuation = if count < items.len() {
                    Some(&items[count - 1].after)
                } else {
                    next.as_ref()
                };
                let next_cursor = continuation
                    .map(|position| make_cursor(&generation, position.next_ordinal, position))
                    .transpose()?;
                let encoded = encode(
                    &generation,
                    complete && continuation.is_none(),
                    NavigationPage::Previews {
                        items: views,
                        next_cursor,
                    },
                )?;
                if encoded.len() <= MAX_ENVELOPE || count <= 1 {
                    break encoded;
                }
                count = count.div_ceil(2);
            }
        }
    };
    if bytes.len() > MAX_ENVELOPE {
        return Err(AppError::PayloadTooLarge.into());
    }
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}

pub(in crate::entry) async fn plan(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
) -> Result<Json<HistoryPlanResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let run: RunId = parse(&run)?;
    let session: SessionKey = parse(&session)?;
    let source = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let loaded = entry
        .file_history
        .plan(source.value)
        .await?
        .ok_or(AppError::Unavailable)?;
    entry
        .store
        .history_session(actor, run.clone(), session.clone())
        .await?;
    let plan = loaded
        .entry
        .map(|(id, part)| {
            Ok::<_, AppError>(PlanEntry {
                entry_id: HistoryEntryIdDto::new(id.as_str()).ok_or(AppError::Internal)?,
                part: visible_part(&entry.auth.endpoint, &run, &session, &part)?,
            })
        })
        .transpose()?;
    Ok(Json(success(
        HistoryPlan {
            run_id: RunIdDto::new(run.as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)?,
            generation: HistoryGenerationDto::new(loaded.generation).ok_or(AppError::Internal)?,
            availability: HistoryAvailability::Available,
            plan,
        },
        source.cursor,
    )?))
}

fn valid_count(value: u64) -> Result<SafeCountDto, AppError> {
    SafeCountDto::new(value).ok_or(AppError::Internal)
}
