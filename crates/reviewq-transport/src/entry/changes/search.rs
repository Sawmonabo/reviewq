use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::changes::{PatchSearchCursor, PatchSearchMatch, PatchSearchRequest, PatchSide};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ApiError, Entry, EventCursorDto, FileIdDto, Page, PageCursorDto, Path, Query, ReviewIdDto,
    SafeCountDto, Success, check_source, dto, parse_review, patch_revision, read_source,
    valid_revision,
};

const SEARCH_CURSOR: &[u8] = b"reviewq-changes-search-v1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::entry) struct SearchQuery {
    revision: String,
    query: String,
    cursor: Option<String>,
    visible_file_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Position {
    review: String,
    revision: String,
    settings: u64,
    query: [u8; 32],
    visible_file: Option<String>,
    page: u32,
    ordinal: u32,
    offset: usize,
    file_id: Option<String>,
    etag: Option<String>,
    checked_visible: bool,
    partial: bool,
    narrow: bool,
}

pub(in crate::entry) async fn search(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<dto::PatchSearchResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    if !valid_revision(&query.revision)
        || query.query.is_empty()
        || query.query.len() > 256
        || query.query.contains('\n')
        || query
            .visible_file_id
            .as_deref()
            .is_some_and(|file| FileIdDto::new(file).is_none())
    {
        return Err(AppError::InvalidRequest.into());
    }
    let source = read_source(&entry, &actor, &review_id).await?;
    let settings = source.context.revision.get();
    let fingerprint: [u8; 32] = Sha256::digest(query.query.as_bytes()).into();
    let endpoint = &entry.auth.endpoint;
    let cursor = if let Some(cursor) = &query.cursor {
        let position: Position = endpoint.decode_cursor(SEARCH_CURSOR, cursor)?;
        if position.review != review
            || position.revision != query.revision
            || position.settings != settings
            || position.query != fingerprint
            || position.visible_file != query.visible_file_id
        {
            return Err(AppError::CursorExpired.into());
        }
        Some(PatchSearchCursor {
            page: position.page,
            ordinal: position.ordinal,
            offset: position.offset,
            file_id: position.file_id,
            etag: position.etag,
            checked_visible: position.checked_visible,
            partial: position.partial,
            narrow: position.narrow,
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
        .search(
            source.value.identity.clone(),
            PatchSearchRequest {
                revision: query.revision,
                query: query.query,
                visible_file: query.visible_file_id.clone(),
                cursor,
            },
        )
        .await?;
    check_source(&entry, &actor, &review_id, &source).await?;
    let coverage = match (response.next.is_some(), response.partial) {
        (true, _) => dto::PatchSearchCoverage::Reading,
        (false, true) => dto::PatchSearchCoverage::Limited,
        (false, false) => dto::PatchSearchCoverage::Complete,
    };
    let next = response
        .next
        .map(|next| {
            let position = Position {
                review: review.clone(),
                revision: response.revision.id.clone(),
                settings,
                query: fingerprint,
                visible_file: query.visible_file_id,
                page: next.page,
                ordinal: next.ordinal,
                offset: next.offset,
                file_id: next.file_id,
                etag: next.etag,
                checked_visible: next.checked_visible,
                partial: next.partial,
                narrow: next.narrow,
            };
            PageCursorDto::new(endpoint.encode_cursor(SEARCH_CURSOR, &position)?)
                .ok_or(AppError::Internal)
        })
        .transpose()?;
    let items = response
        .items
        .into_iter()
        .map(hit)
        .collect::<Result<_, _>>()?;
    Ok(Json(Success {
        cursor: EventCursorDto::new(source.cursor.get()).ok_or(AppError::Internal)?,
        data: dto::PatchSearch {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            revision: patch_revision(&response.revision.id)?,
            page: Page::new(items, next).ok_or(AppError::Internal)?,
            coverage,
        },
    }))
}

fn hit(hit: PatchSearchMatch) -> Result<dto::PatchSearchMatch, AppError> {
    let count = |value: usize| SafeCountDto::new(value as u64).ok_or(AppError::Internal);
    Ok(dto::PatchSearchMatch {
        file_id: FileIdDto::new(hit.file_id).ok_or(AppError::Internal)?,
        old_path: hit.old_path,
        new_path: hit.new_path,
        side: match hit.side {
            PatchSide::Old => dto::PatchSide::Old,
            PatchSide::New => dto::PatchSide::New,
        },
        line: count(hit.line as usize)?,
        hunk: count(hit.hunk)?,
        source_start: count(hit.source_start)?,
        source_end: count(hit.source_end)?,
        snippet: hit.snippet,
        highlight_start: count(hit.highlight_start)?,
        highlight_end: count(hit.highlight_end)?,
    })
}
