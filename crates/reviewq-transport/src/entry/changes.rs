use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::changes::{
    Discussion, DiscussionAnchor, DiscussionLine, DiscussionPosition, DiscussionResolution,
    PatchAvailability, PatchFile, PatchFileSelection, PatchPagePreview,
};
use reviewq_app::queue::ReviewSource;
use reviewq_app::settings::ActiveSource;
use reviewq_domain::identity::ReviewId;
use reviewq_protocol::changes as dto;
use reviewq_protocol::{
    EventCursorDto, FileIdDto, GitShaDto, Page, PageCursorDto, PatchETagDto, PatchRevisionDto,
    ReviewIdDto, RevisionDto, SafeCountDto, Success, UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use super::error::{ApiPath as Path, ApiQuery as Query};
use super::{ApiError, Entry, ServiceEndpoint};

mod approval;
mod discussion_text;
mod paths;
mod raw;
mod search;
mod selection;
mod source;
pub(super) use approval::approvals;
pub(super) use paths::paths;
pub(super) use raw::patch;
pub(super) use search::search;
pub(super) use selection::selected;
pub(super) use source::source;

const FILE_CURSOR: &[u8] = b"reviewq-changes-files-v1";
const DISCUSSION_CURSOR: &[u8] = b"reviewq-changes-discussions-v1";
const DISCUSSION_LIMIT: u16 = 40;
const DISCUSSION_SMALL_LIMIT: u16 = 20;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ChangeQuery {
    revision: Option<String>,
    limit: Option<u16>,
    cursor: Option<String>,
    preferred_file_id: Option<String>,
    file_path_hint: Option<String>,
    preview_edge: Option<String>,
    include_selected_preview: Option<u8>,
}

#[derive(Deserialize, Serialize)]
struct Position {
    review: String,
    settings: u64,
    revision: String,
    page: u32,
    limit: u16,
}

pub(super) async fn revision(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
) -> Result<Json<dto::CurrentPatchRevisionResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    let source = read_source(&entry, &actor, &review_id).await?;
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let current = provider.revision(source.value.identity.clone()).await?;
    check_source(&entry, &actor, &review_id, &source).await?;
    Ok(Json(Success {
        cursor: EventCursorDto::new(source.cursor.get()).ok_or(AppError::Internal)?,
        data: dto::CurrentPatchRevision {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            revision: patch_revision(&current.id)?,
            settings_revision: Some(
                RevisionDto::new(source.context.revision.get()).ok_or(AppError::Internal)?,
            ),
        },
    }))
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
    if query.file_path_hint.is_some()
        && (query.preferred_file_id.is_some() || query.cursor.is_some() || query.revision.is_none())
    {
        return Err(AppError::InvalidRequest.into());
    }
    let selected = query
        .file_path_hint
        .clone()
        .map(PatchFileSelection::PathHint)
        .or_else(|| {
            query
                .preferred_file_id
                .clone()
                .map(PatchFileSelection::Exact)
        });
    if selected
        .as_ref()
        .is_some_and(|file| file.key().is_empty() || file.key().len() > 80)
    {
        return Err(AppError::InvalidRequest.into());
    }
    let preview = match (
        query.preview_edge.as_deref(),
        query.include_selected_preview,
    ) {
        (None, None) => None,
        (None, Some(1)) if query.cursor.is_none() => Some(PatchPagePreview::Selected),
        (Some("start"), None) if query.cursor.is_some() => Some(PatchPagePreview::Start),
        (Some("end"), None) if query.cursor.is_some() => Some(PatchPagePreview::End),
        _ => return Err(AppError::InvalidRequest.into()),
    };
    let (limit, page, revision) = query.position(
        &entry.auth.endpoint,
        FILE_CURSOR,
        &review_id,
        source.context.revision.get(),
        10,
    )?;
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let response = provider
        .files(
            source.value.identity.clone(),
            revision,
            page,
            limit,
            selected,
            preview,
        )
        .await?;
    check_source(&entry, &actor, &review_id, &source).await?;
    let position = |page| Position {
        review: review.clone(),
        settings: source.context.revision.get(),
        revision: response.revision.id.clone(),
        page,
        limit,
    };
    let next = encode(
        &entry.auth.endpoint,
        FILE_CURSOR,
        response.next_page.map(position),
    )?;
    let previous = (response.page > 1).then(|| position(response.page - 1));
    let previous_cursor = encode(&entry.auth.endpoint, FILE_CURSOR, previous)?;
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
            settings_revision: Some(
                RevisionDto::new(source.context.revision.get()).ok_or(AppError::Internal)?,
            ),
            base_sha: GitShaDto::new(response.revision.base.as_str()).ok_or(AppError::Internal)?,
            start_sha: GitShaDto::new(response.revision.start.as_str())
                .ok_or(AppError::Internal)?,
            head_sha: GitShaDto::new(response.revision.head.as_str()).ok_or(AppError::Internal)?,
            page_number: SafeCountDto::new(u64::from(response.page)).ok_or(AppError::Internal)?,
            previous_cursor,
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
    if query.preferred_file_id.is_some()
        || query.file_path_hint.is_some()
        || query.preview_edge.is_some()
        || query.include_selected_preview.is_some()
    {
        return Err(AppError::InvalidRequest.into());
    }
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    let source = read_source(&entry, &actor, &review_id).await?;
    let include_text_projection = query.cursor.is_none()
        && headers
            .get("x-reviewq-discussion-text")
            .is_some_and(|value| value.as_bytes() == b"1");
    let adaptive = query.limit.is_none();
    let (mut limit, page, revision) = query.position(
        &entry.auth.endpoint,
        DISCUSSION_CURSOR,
        &review_id,
        source.context.revision.get(),
        DISCUSSION_LIMIT,
    )?;
    let revision = revision.ok_or(AppError::InvalidRequest)?;
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let identity = &source.value.identity;
    let response = match provider
        .discussions(identity.clone(), revision.clone(), page, limit)
        .await
    {
        Err(AppError::PayloadTooLarge) if adaptive && limit == DISCUSSION_LIMIT => {
            // Both limits divide the same upstream offset, including after a cursor.
            let factor = u32::from(DISCUSSION_LIMIT / DISCUSSION_SMALL_LIMIT);
            let small_page = page
                .checked_sub(1)
                .and_then(|offset| offset.checked_mul(factor))
                .and_then(|offset| offset.checked_add(1))
                .ok_or(AppError::InvalidRequest)?;
            limit = DISCUSSION_SMALL_LIMIT;
            provider
                .discussions(identity.clone(), revision, small_page, limit)
                .await?
        }
        response => response?,
    };
    let position = response.next_page.map(|page| Position {
        review: review.clone(),
        settings: source.context.revision.get(),
        revision: response.revision.id.clone(),
        page,
        limit,
    });
    let next = encode(&entry.auth.endpoint, DISCUSSION_CURSOR, position)?;
    let web_url = source.value.web_url()?;
    let items = response
        .items
        .into_iter()
        .map(|row| discussion(row, &web_url, &source.context))
        .collect::<Result<Vec<_>, _>>()?;
    let text_projections = if include_text_projection {
        discussion_text::project_page(&items).await
    } else {
        None
    };
    check_source(&entry, &actor, &review_id, &source).await?;
    Ok(Json(Success {
        cursor: EventCursorDto::new(source.cursor.get()).ok_or(AppError::Internal)?,
        data: dto::PatchDiscussions {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            revision: patch_revision(&response.revision.id)?,
            page: Page::new(items, next).ok_or(AppError::Internal)?,
            text_projections,
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
        settings: u64,
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
        let adaptive_discussion = scope == DISCUSSION_CURSOR
            && self.limit.is_none()
            && matches!(position.limit, DISCUSSION_LIMIT | DISCUSSION_SMALL_LIMIT);
        if position.review != review.as_str()
            || position.settings != settings
            || !(1..=max_limit).contains(&position.limit)
            || (!adaptive_discussion && position.limit != limit)
            || self.revision.as_deref() != Some(&position.revision)
        {
            return Err(AppError::CursorExpired);
        }
        Ok((position.limit, position.page, Some(position.revision)))
    }
}

struct SelectedSource {
    value: ReviewSource,
    cursor: reviewq_domain::identity::EventCursor,
    context: ActiveSource,
}

async fn read_source(
    entry: &Entry,
    actor: &Actor,
    review: &ReviewId,
) -> Result<SelectedSource, AppError> {
    let context = entry.active_source().await?;
    let snapshot = entry
        .store
        .authorized_source(actor.clone(), review.clone())
        .await?;
    let (settings, identity) = snapshot.value;
    if context.revision != settings {
        return Err(AppError::StaleRevision);
    }
    Ok(SelectedSource {
        value: identity,
        cursor: snapshot.cursor,
        context,
    })
}

async fn check_source(
    entry: &Entry,
    actor: &Actor,
    review: &ReviewId,
    source: &SelectedSource,
) -> Result<(), AppError> {
    let current = read_source(entry, actor, review).await?;
    if current.context.revision != source.context.revision {
        return Err(AppError::StaleRevision);
    }
    if current.value.identity != source.value.identity {
        return Err(AppError::NotFound);
    }
    if current.value.project_path != source.value.project_path {
        return Err(AppError::StaleRevision);
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
    let (byte_length, line_count, max_line_bytes, line_changes, patch_etag) = match file.metrics {
        Some(data) => (
            SafeCountDto::new(data.byte_length as u64),
            SafeCountDto::new(data.line_count as u64),
            SafeCountDto::new(data.max_line_bytes as u64),
            data.line_changes,
            PatchETagDto::new(data.etag),
        ),
        None => (None, None, None, None, None),
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
        additions: line_changes.and_then(|changes| SafeCountDto::new(changes.additions as u64)),
        deletions: line_changes.and_then(|changes| SafeCountDto::new(changes.deletions as u64)),
        patch_etag,
        preview: file.preview,
    })
}

fn discussion(
    discussion: Discussion,
    mr_web_url: &str,
    context: &ActiveSource,
) -> Result<dto::PatchDiscussion, AppError> {
    let web_url = discussion.notes.first().map_or_else(
        || mr_web_url.to_string(),
        |first| format!("{mr_web_url}#note_{}", first.id),
    );
    let ours = |id: Option<u64>| {
        id.is_some_and(|id| {
            context.gitlab_account == Some(id) || context.scope.additional_reviewers.contains(&id)
        })
    };
    let our_participation = discussion.notes.iter().any(|note| ours(note.author_id));
    let resolution = match DiscussionResolution::from_notes(
        discussion
            .notes
            .iter()
            .map(|note| (note.resolvable, note.resolved)),
    ) {
        DiscussionResolution::Open => dto::DiscussionResolution::Open,
        DiscussionResolution::Resolved => dto::DiscussionResolution::Resolved,
        DiscussionResolution::Comment => dto::DiscussionResolution::Comment,
        DiscussionResolution::Unknown => dto::DiscussionResolution::Unknown,
    };
    let anchor = discussion.anchor.map(project_anchor).transpose()?;
    let notes = discussion
        .notes
        .into_iter()
        .map(|row| dto::PatchDiscussionNote {
            note_id: row.id.to_string(),
            web_url: format!("{mr_web_url}#note_{}", row.id),
            author: row.author,
            author_id: row.author_id.and_then(reviewq_protocol::UpstreamIdDto::new),
            ours: ours(row.author_id),
            body: row.body,
            created_at: row
                .created_at_ms
                .and_then(UtcTimestampDto::from_unix_milliseconds),
            resolvable: row.resolvable,
            resolved: row.resolved,
            resolved_by: row
                .resolved_by
                .and_then(reviewq_protocol::UpstreamIdDto::new),
        })
        .collect();
    Ok(dto::PatchDiscussion {
        discussion_id: discussion.id,
        anchor,
        outdated: discussion.outdated,
        web_url,
        our_participation,
        resolution,
        notes,
    })
}

fn project_anchor(anchor: DiscussionAnchor) -> Result<dto::PatchDiscussionAnchor, AppError> {
    let position_type = match anchor.position_type {
        DiscussionPosition::Text => dto::DiscussionPosition::Text,
        DiscussionPosition::File => dto::DiscussionPosition::File,
        DiscussionPosition::Other => dto::DiscussionPosition::Other,
    };
    Ok(dto::PatchDiscussionAnchor {
        base_sha: GitShaDto::new(anchor.base_sha.as_str()).ok_or(AppError::Internal)?,
        start_sha: GitShaDto::new(anchor.start_sha.as_str()).ok_or(AppError::Internal)?,
        head_sha: GitShaDto::new(anchor.head_sha.as_str()).ok_or(AppError::Internal)?,
        position_type,
        old_path: anchor.old_path,
        new_path: anchor.new_path,
        old_line: anchor
            .old_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
        new_line: anchor
            .new_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
        range: anchor.range.map(|range| dto::PatchDiscussionRange {
            start: project_line(range.start),
            end: project_line(range.end),
        }),
    })
}

fn project_line(line: DiscussionLine) -> dto::PatchDiscussionLine {
    dto::PatchDiscussionLine {
        old_line: line
            .old_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
        new_line: line
            .new_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
    }
}
