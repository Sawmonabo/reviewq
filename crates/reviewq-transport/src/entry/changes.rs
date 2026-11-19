use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::changes::{Discussion, PatchAvailability, PatchFile};
use reviewq_app::events::Snapshot;
use reviewq_domain::identity::{MrIdentity, ReviewId};
use reviewq_protocol::changes as dto;
use reviewq_protocol::{
    EventCursorDto, FileIdDto, GitShaDto, Page, PageCursorDto, PatchETagDto, PatchRevisionDto,
    ReviewIdDto, SafeCountDto, Success, UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use super::error::{ApiPath as Path, ApiQuery as Query};
use super::{ApiError, Entry, ServiceEndpoint};

mod raw;
pub(super) use raw::patch;

const FILE_CURSOR: &[u8] = b"reviewq-changes-files-v1";
const DISCUSSION_CURSOR: &[u8] = b"reviewq-changes-discussions-v1";

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ChangeQuery {
    revision: Option<String>,
    limit: Option<u16>,
    cursor: Option<String>,
    preferred_file_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Position {
    review: String,
    revision: String,
    page: u32,
    limit: u16,
}

pub(super) async fn files(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<ChangeQuery>,
) -> Result<Json<dto::PatchFileIndexResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    let source = read_source(&entry, &actor, &review_id).await?;
    let selected = query.preferred_file_id.clone();
    if selected
        .as_deref()
        .is_some_and(|id| id.is_empty() || id.len() > 80)
    {
        return Err(AppError::InvalidRequest.into());
    }
    let (limit, page, revision) =
        query.position(&entry.auth.endpoint, FILE_CURSOR, &review_id, 10)?;
    let provider = entry.source_reader().await?;
    let response = provider
        .files(source.value.clone(), revision, page, limit, selected)
        .await?;
    check_source(&entry, &actor, &review_id, &source.value).await?;
    let position = response.next_page.map(|page| Position {
        review: review.clone(),
        revision: response.revision.id.clone(),
        page,
        limit,
    });
    let next = encode(&entry.auth.endpoint, FILE_CURSOR, position)?;
    let revision = patch_revision(&response.revision.id)?;
    let items = response
        .items
        .into_iter()
        .map(|file| file_metadata(&review, &revision, file))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(Success {
        cursor: EventCursorDto::new(source.cursor.get()).ok_or(AppError::Internal)?,
        data: dto::PatchFileIndex {
            revision,
            base_sha: GitShaDto::new(response.revision.base.as_str()).ok_or(AppError::Internal)?,
            start_sha: GitShaDto::new(response.revision.start.as_str())
                .ok_or(AppError::Internal)?,
            head_sha: GitShaDto::new(response.revision.head.as_str()).ok_or(AppError::Internal)?,
            page: Page::new(items, next).ok_or(AppError::Internal)?,
        },
    }))
}

pub(super) async fn discussions(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<ChangeQuery>,
) -> Result<Json<dto::PatchDiscussionsResponse>, ApiError> {
    if query.preferred_file_id.is_some() {
        return Err(AppError::InvalidRequest.into());
    }
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    let source = read_source(&entry, &actor, &review_id).await?;
    let (limit, page, revision) =
        query.position(&entry.auth.endpoint, DISCUSSION_CURSOR, &review_id, 20)?;
    let revision = revision.ok_or(AppError::InvalidRequest)?;
    let provider = entry.source_reader().await?;
    let response = provider
        .discussions(source.value.clone(), revision, page, limit)
        .await?;
    check_source(&entry, &actor, &review_id, &source.value).await?;
    let position = response.next_page.map(|page| Position {
        review: review.clone(),
        revision: response.revision.id.clone(),
        page,
        limit,
    });
    let next = encode(&entry.auth.endpoint, DISCUSSION_CURSOR, position)?;
    let items = response.items.into_iter().map(discussion).collect();
    Ok(Json(Success {
        cursor: EventCursorDto::new(source.cursor.get()).ok_or(AppError::Internal)?,
        data: dto::PatchDiscussions {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            revision: patch_revision(&response.revision.id)?,
            page: Page::new(items, next).ok_or(AppError::Internal)?,
        },
    }))
}

fn encode(
    endpoint: &ServiceEndpoint,
    scope: &[u8],
    position: Option<Position>,
) -> Result<Option<PageCursorDto>, AppError> {
    position
        .map(|position| {
            PageCursorDto::new(endpoint.encode_cursor(scope, &position)?).ok_or(AppError::Internal)
        })
        .transpose()
}

impl ChangeQuery {
    fn position(
        self,
        endpoint: &ServiceEndpoint,
        scope: &[u8],
        review: &ReviewId,
        max_limit: u16,
    ) -> Result<(u16, u32, Option<String>), AppError> {
        let limit = self.limit.unwrap_or(max_limit);
        if !(1..=max_limit).contains(&limit) {
            return Err(AppError::InvalidRequest);
        }
        if self
            .revision
            .as_deref()
            .is_some_and(|value| !valid_revision(value))
        {
            return Err(AppError::InvalidRequest);
        }
        let Some(cursor) = self.cursor.as_deref() else {
            return Ok((limit, 1, self.revision));
        };
        let position: Position = endpoint.decode_cursor(scope, cursor)?;
        if position.review != review.as_str()
            || position.limit != limit
            || self.revision.as_deref() != Some(&position.revision)
        {
            return Err(AppError::CursorExpired);
        }
        Ok((limit, position.page, Some(position.revision)))
    }
}

async fn read_source(
    entry: &Entry,
    actor: &Actor,
    review: &ReviewId,
) -> Result<Snapshot<MrIdentity>, AppError> {
    let snapshot = entry
        .store
        .read_review(actor.clone(), review.clone(), None, 1)
        .await?;
    Ok(Snapshot {
        value: snapshot.value.item.source.identity,
        cursor: snapshot.cursor,
    })
}

async fn check_source(
    entry: &Entry,
    actor: &Actor,
    review: &ReviewId,
    source: &MrIdentity,
) -> Result<(), AppError> {
    if read_source(entry, actor, review).await?.value != *source {
        return Err(AppError::NotFound);
    }
    Ok(())
}

fn parse_review(value: &str) -> Result<ReviewId, AppError> {
    value.parse().map_err(|_| AppError::InvalidRequest)
}

fn valid_revision(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn patch_revision(value: &str) -> Result<PatchRevisionDto, AppError> {
    PatchRevisionDto::new(value).ok_or(AppError::Internal)
}

fn file_metadata(
    review: &str,
    revision: &PatchRevisionDto,
    file: PatchFile,
) -> Result<dto::PatchFile, AppError> {
    let availability = match file.availability {
        PatchAvailability::Complete => dto::PatchAvailability::Complete,
        PatchAvailability::Incomplete => dto::PatchAvailability::Incomplete,
        PatchAvailability::Binary => dto::PatchAvailability::Binary,
        PatchAvailability::ProviderLimited => dto::PatchAvailability::ProviderLimited,
    };
    let (byte_length, line_count, max_line_bytes, patch_etag) = match file.metrics {
        Some(data) => (
            SafeCountDto::new(data.byte_length as u64),
            SafeCountDto::new(data.line_count as u64),
            SafeCountDto::new(data.max_line_bytes as u64),
            PatchETagDto::new(data.etag),
        ),
        None => (None, None, None, None),
    };
    Ok(dto::PatchFile {
        review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
        revision: revision.clone(),
        file_id: FileIdDto::new(file.id).ok_or(AppError::Internal)?,
        old_path: file.old_path,
        new_path: file.new_path,
        renamed: file.renamed,
        created: file.created,
        deleted: file.deleted,
        availability,
        byte_length,
        line_count,
        max_line_bytes,
        patch_etag,
    })
}

fn discussion(discussion: Discussion) -> dto::PatchDiscussion {
    let anchor = discussion.anchor.map(|anchor| dto::PatchDiscussionAnchor {
        old_path: anchor.old_path,
        new_path: anchor.new_path,
        old_line: anchor
            .old_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
        new_line: anchor
            .new_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
    });
    let notes = discussion
        .notes
        .into_iter()
        .map(|row| dto::PatchDiscussionNote {
            note_id: row.id.to_string(),
            author: row.author,
            body: row.body,
            created_at: row
                .created_at_ms
                .and_then(UtcTimestampDto::from_unix_milliseconds),
            resolved: row.resolved,
        })
        .collect();
    dto::PatchDiscussion {
        discussion_id: discussion.id,
        anchor,
        outdated: discussion.outdated,
        notes,
    }
}
