use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::changes::{
    PATCH_SOURCE_BYTES, PatchSide, PatchSourceRequest, PatchSourceRow, PatchSourceTarget,
};
use serde::Deserialize;

use super::{
    ApiError, Entry, EventCursorDto, FileIdDto, GitShaDto, PatchETagDto, Path, Query, ReviewIdDto,
    SafeCountDto, check_source, dto, parse_review, patch_revision, read_source, valid_revision,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::entry) struct SourceQuery {
    revision: String,
    file_id: String,
    row: Option<usize>,
    byte: Option<usize>,
    side: Option<Side>,
    line: Option<u32>,
    before: Option<usize>,
    last: Option<u8>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Side {
    Old,
    New,
}

impl SourceQuery {
    fn target(&self) -> Result<PatchSourceTarget, AppError> {
        match (
            self.row,
            self.byte,
            &self.side,
            self.line,
            self.before,
            self.last,
        ) {
            (Some(row), byte, None, None, None, None) => Ok(PatchSourceTarget::Row { row, byte }),
            (None, Some(byte), None, None, None, None) => Ok(PatchSourceTarget::Byte(byte)),
            (None, None, Some(side), Some(line), None, None) if line > 0 => {
                let side = match side {
                    Side::Old => PatchSide::Old,
                    Side::New => PatchSide::New,
                };
                Ok(PatchSourceTarget::Line { side, line })
            }
            (None, None, None, None, Some(before), None) => Ok(PatchSourceTarget::Before(before)),
            (None, None, None, None, None, Some(1)) => Ok(PatchSourceTarget::End),
            _ => Err(AppError::InvalidRequest),
        }
    }
}

pub(in crate::entry) async fn source(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<SourceQuery>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    if !valid_revision(&query.revision) || FileIdDto::new(query.file_id.as_str()).is_none() {
        return Err(AppError::InvalidRequest.into());
    }
    let target = query.target()?;
    let mut matches = headers.get_all(header::IF_MATCH).iter();
    let if_match = matches
        .next()
        .and_then(|value| value.to_str().ok())
        .filter(|_| matches.next().is_none())
        .and_then(PatchETagDto::new)
        .ok_or(AppError::PreconditionFailed)?;
    let selected = read_source(&entry, &actor, &review_id).await?;
    let provider = selected
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let page = provider
        .patch_source(
            selected.value.identity.clone(),
            PatchSourceRequest {
                revision: query.revision,
                file_id: query.file_id,
                etag: if_match.as_str().to_owned(),
                target,
            },
        )
        .await?;
    check_source(&entry, &actor, &review_id, &selected).await?;
    let response = dto::PatchSourceResponse {
        cursor: EventCursorDto::new(selected.cursor.get()).ok_or(AppError::Internal)?,
        data: dto::PatchSourcePage {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            revision: patch_revision(&page.revision.id)?,
            file_id: FileIdDto::new(page.file_id).ok_or(AppError::Internal)?,
            base_sha: GitShaDto::new(page.revision.base.to_string()).ok_or(AppError::Internal)?,
            start_sha: GitShaDto::new(page.revision.start.to_string()).ok_or(AppError::Internal)?,
            head_sha: GitShaDto::new(page.revision.head.to_string()).ok_or(AppError::Internal)?,
            patch_etag: PatchETagDto::new(page.etag.as_str()).ok_or(AppError::Internal)?,
            total_bytes: count(page.window.total.byte)?,
            total_rows: count(page.window.total.row)?,
            rows: page
                .window
                .rows
                .into_iter()
                .map(source_row)
                .collect::<Result<_, _>>()?,
            next: page
                .window
                .next
                .map(|next| {
                    Ok::<_, AppError>(dto::PatchSourceCursor {
                        row: count(next.row)?,
                        byte: count(next.byte)?,
                    })
                })
                .transpose()?,
        },
    };
    let bytes = serde_json::to_vec(&response).map_err(|_| AppError::Internal)?;
    if bytes.len() > PATCH_SOURCE_BYTES {
        return Err(AppError::Internal.into());
    }
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&page.etag).map_err(|_| AppError::Internal)?,
    );
    Ok(response)
}

fn source_row(row: PatchSourceRow) -> Result<dto::PatchSourceRow, AppError> {
    Ok(dto::PatchSourceRow {
        row: count(row.row)?,
        hunk: row.hunk.map(count).transpose()?,
        old_line: row
            .old_line
            .map(|value| count(value as usize))
            .transpose()?,
        new_line: row
            .new_line
            .map(|value| count(value as usize))
            .transpose()?,
        row_start: count(row.row_start)?,
        row_end: count(row.row_end)?,
        source_start: count(row.source_start)?,
        source_end: count(row.source_end)?,
        text: row.text,
    })
}

fn count(value: usize) -> Result<SafeCountDto, AppError> {
    SafeCountDto::new(value as u64).ok_or(AppError::Internal)
}
