use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::changes::PatchPathCursor;
use reviewq_protocol::changes as dto;
use reviewq_protocol::{EventCursorDto, FileIdDto, Page, PageCursorDto, ReviewIdDto, Success};
use serde::{Deserialize, Serialize};

use super::{
    ApiError, Entry, Path, Query, check_source, parse_review, patch_revision, read_source,
    valid_revision,
};

const PATH_CURSOR: &[u8] = b"reviewq-changes-paths-v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::entry) struct PathQuery {
    revision: String,
    cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Position {
    review_id: String,
    settings_revision: u64,
    revision: String,
    page: u32,
    after: String,
    partial: bool,
}

pub(in crate::entry) async fn paths(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<dto::PatchPathsResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    if !valid_revision(&query.revision) {
        return Err(AppError::InvalidRequest.into());
    }
    let source = read_source(&entry, &actor, &review_id).await?;
    let settings_revision = source.context.revision.get();
    let cursor = if let Some(token) = query.cursor {
        let position: Position = entry.auth.endpoint.decode_cursor(PATH_CURSOR, &token)?;
        if position.review_id != review
            || position.settings_revision != settings_revision
            || position.revision != query.revision
        {
            return Err(AppError::CursorExpired.into());
        }
        Some(PatchPathCursor {
            page: position.page,
            after: position.after,
            partial: position.partial,
        })
    } else {
        None
    };
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let response = provider
        .paths(
            source.value.identity.clone(),
            source.value.project_path.clone(),
            query.revision,
            cursor,
        )
        .await?;
    check_source(&entry, &actor, &review_id, &source).await?;
    let next = response
        .next
        .map(|cursor| {
            let position = Position {
                review_id: review.clone(),
                settings_revision,
                revision: response.revision.id.clone(),
                page: cursor.page,
                after: cursor.after,
                partial: cursor.partial,
            };
            PageCursorDto::new(entry.auth.endpoint.encode_cursor(PATH_CURSOR, &position)?)
                .ok_or(AppError::Internal)
        })
        .transpose()?;
    let items = response
        .items
        .into_iter()
        .map(|path| {
            Ok(dto::PatchPath {
                old_path: path.old_path,
                new_path: path.new_path,
                lookup_key: FileIdDto::new(path.lookup_key).ok_or(AppError::Internal)?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(Success {
        cursor: EventCursorDto::new(source.cursor.get()).ok_or(AppError::Internal)?,
        data: dto::PatchPaths {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            revision: patch_revision(&response.revision.id)?,
            partial: response.partial,
            page: Page::new(items, next).ok_or(AppError::Internal)?,
        },
    }))
}
