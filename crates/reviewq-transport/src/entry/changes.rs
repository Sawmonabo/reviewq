use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::changes::{
    Discussion, DiscussionAnchor, DiscussionInvalidation, DiscussionLine, DiscussionPage,
    DiscussionPageRequest, DiscussionPosition, DiscussionResolution, DiscussionTarget, NoteAddress,
    PatchAvailability, PatchFile, PatchFileSelection, PatchPagePreview,
};
use reviewq_app::queue::ReviewSource;
use reviewq_app::settings::ActiveSource;
use reviewq_domain::identity::{EventCursor, ReviewId};
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
mod notes;
mod paths;
mod raw;
mod search;
mod selection;
mod source;
pub(super) use approval::approvals;
pub(super) use notes::{note_destination, search_notes};
pub(super) use paths::paths;
pub(super) use raw::patch;
pub(super) use search::search;
pub(super) use selection::selected;
pub(super) use source::source;

const FILE_CURSOR: &[u8] = b"reviewq-changes-files-v1";
const DISCUSSION_CURSOR: &[u8] = b"reviewq-changes-discussions-v3";
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

#[derive(Deserialize, Serialize)]
struct DiscussionCursor {
    scope: notes::Scope,
    position: DiscussionPagePosition,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct DiscussionPagePosition {
    offset: u32,
    limit: u16,
    narrow_at: Option<u32>,
}

impl DiscussionPagePosition {
    fn page(self) -> Result<u32, AppError> {
        (self.offset / u32::from(self.limit))
            .checked_add(1)
            .ok_or(AppError::InvalidRequest)
    }

    fn number(self) -> u64 {
        match self.narrow_at.filter(|offset| *offset <= self.offset) {
            Some(cut) => {
                u64::from(cut / u32::from(DISCUSSION_LIMIT))
                    + u64::from((self.offset - cut) / u32::from(DISCUSSION_SMALL_LIMIT))
                    + 1
            }
            None => u64::from(self.offset / u32::from(self.limit)) + 1,
        }
    }

    fn resized(self, limit: u16) -> Self {
        if limit == self.limit {
            return self;
        }
        Self {
            limit,
            narrow_at: Some(self.offset),
            ..self
        }
    }

    fn previous(self) -> Option<Self> {
        let limit = if self.narrow_at == Some(self.offset) {
            DISCUSSION_LIMIT
        } else {
            self.limit
        };
        Some(Self {
            offset: self.offset.checked_sub(u32::from(limit))?,
            limit,
            ..self
        })
    }

    fn next(self) -> Option<Self> {
        let offset = self.offset.checked_add(u32::from(self.limit))?;
        let limit = if self.narrow_at == Some(offset) {
            DISCUSSION_SMALL_LIMIT
        } else {
            self.limit
        };
        Some(Self {
            offset,
            limit,
            ..self
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DiscussionQuery {
    revision: String,
    limit: Option<u16>,
    cursor: Option<String>,
    #[serde(default)]
    recheck: bool,
    anchor: Option<String>,
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
    Query(query): Query<DiscussionQuery>,
) -> Result<Json<dto::PatchDiscussionsResponse>, ApiError> {
    if !valid_revision(&query.revision)
        || (query.recheck && query.cursor.is_none())
        || query.anchor.as_ref().is_some_and(|id| {
            !query.recheck || id.is_empty() || id.len() > 256 || id.chars().any(char::is_control)
        })
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
    let epoch = entry
        .store
        .discussion_epoch(review_id.clone(), source.context.revision)
        .await?
        .cursor;
    let requested = query.limit.unwrap_or(DISCUSSION_LIMIT);
    if !(1..=DISCUSSION_LIMIT).contains(&requested) {
        return Err(AppError::InvalidRequest.into());
    }
    let mut scope = notes::Scope::new(&review_id, &source, query.revision, epoch);
    let position = if let Some(cursor) = query.cursor {
        let saved: DiscussionCursor = entry
            .auth
            .endpoint
            .decode_cursor(DISCUSSION_CURSOR, &cursor)?;
        if query.recheck {
            // Open the same signed coordinate with today's authorized observation.
            scope.check_authority(&saved.scope)?;
        } else {
            scope = scope.continued(saved.scope)?;
        }
        if saved.position.limit != requested
            && !(query.limit.is_none() && saved.position.limit == DISCUSSION_SMALL_LIMIT)
        {
            return Err(AppError::CursorExpired.into());
        }
        saved.position
    } else {
        DiscussionPagePosition {
            offset: 0,
            limit: requested,
            narrow_at: None,
        }
    };
    let read = async {
        let (response, limit) = read_discussion_page(
            &source,
            &scope,
            position.page()?,
            position.limit,
            query.limit.is_none(),
        )
        .await?;
        scope.observation = Some(response.observation.clone());
        let anchor = match query.anchor {
            Some(id) if !response.items.iter().any(|thread| thread.id == id) => {
                recover_visible_discussion(&source, &scope, id).await?
            }
            _ => None,
        };
        Ok((response, limit, anchor))
    };
    let (response, limit, anchor) = match read.await {
        Err(error) if loses_access(&error) => {
            entry
                .store
                .invalidate_discussions(
                    review_id,
                    source.context.revision,
                    epoch,
                    DiscussionInvalidation::Denied,
                )
                .await?;
            return Err(error.into());
        }
        result => result?,
    };
    let position = position.resized(limit);
    let cursor = |position: Option<DiscussionPagePosition>| {
        encode(
            &entry.auth.endpoint,
            DISCUSSION_CURSOR,
            position.map(|position| DiscussionCursor {
                scope: scope.clone(),
                position,
            }),
        )
    };
    let next = cursor(response.next_page.and_then(|_| position.next()))?;
    let previous_cursor = cursor(position.previous())?;
    let web_url = source.value.web_url()?;
    let items = anchor
        .iter()
        .chain(response.items.iter())
        .map(|row| discussion(row, &web_url, &source.context))
        .collect::<Result<Vec<_>, _>>()?;
    let text_projections = if include_text_projection {
        discussion_text::project_page(&items).await
    } else {
        None
    };
    check_discussions(&entry, &actor, &review_id, &source, epoch).await?;
    Ok(Json(Success {
        cursor: EventCursorDto::new(source.cursor.get()).ok_or(AppError::Internal)?,
        data: dto::PatchDiscussions {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            revision: patch_revision(&response.revision.id)?,
            page_number: SafeCountDto::new(position.number()).ok_or(AppError::Internal)?,
            previous_cursor,
            page: Page::new(items, next).ok_or(AppError::Internal)?,
            text_projections,
        },
    }))
}

async fn recover_visible_discussion(
    source: &SelectedSource,
    scope: &notes::Scope,
    id: String,
) -> Result<Option<Discussion>, AppError> {
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let result = provider
        .discussion_target(
            source.value.identity.clone(),
            scope.provider()?,
            NoteAddress::Published(id),
        )
        .await;
    match result {
        Ok(DiscussionTarget::Published(thread)) => Ok(Some(thread)),
        Err(AppError::NotFound | AppError::PayloadTooLarge) => Ok(None),
        Err(error) => Err(error),
        Ok(DiscussionTarget::Draft(_)) => Err(AppError::Internal),
    }
}

async fn read_discussion_page(
    source: &SelectedSource,
    scope: &notes::Scope,
    page: u32,
    limit: u16,
    adaptive: bool,
) -> Result<(DiscussionPage, u16), AppError> {
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let identity = &source.value.identity;
    let request = |page, limit| -> Result<DiscussionPageRequest, AppError> {
        Ok(DiscussionPageRequest {
            scope: scope.provider()?,
            page,
            limit,
            allow_partial: false,
        })
    };
    match provider
        .discussions(identity.clone(), request(page, limit)?)
        .await
    {
        Err(AppError::PayloadTooLarge) if adaptive && limit == DISCUSSION_LIMIT => {
            // Both limits divide the same upstream offset, including after a cursor.
            let factor = u32::from(DISCUSSION_LIMIT / DISCUSSION_SMALL_LIMIT);
            let page = page
                .checked_sub(1)
                .and_then(|offset| offset.checked_mul(factor))
                .and_then(|offset| offset.checked_add(1))
                .ok_or(AppError::InvalidRequest)?;
            let response = provider
                .discussions(identity.clone(), request(page, DISCUSSION_SMALL_LIMIT)?)
                .await?;
            Ok((response, DISCUSSION_SMALL_LIMIT))
        }
        result => Ok((result?, limit)),
    }
}

async fn check_discussions(
    entry: &Entry,
    actor: &Actor,
    review: &ReviewId,
    source: &SelectedSource,
    epoch: EventCursor,
) -> Result<(), AppError> {
    check_source(entry, actor, review, source).await?;
    let current = entry
        .store
        .discussion_epoch(review.clone(), source.context.revision)
        .await?;
    if current.cursor != epoch {
        return Err(AppError::CursorExpired);
    }
    if current.denied {
        entry
            .store
            .invalidate_discussions(
                review.clone(),
                source.context.revision,
                epoch,
                DiscussionInvalidation::Restored,
            )
            .await?;
        return Err(AppError::CursorExpired);
    }
    Ok(())
}

fn loses_access(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Forbidden | AppError::Unauthorized | AppError::NotFound
    )
}

fn encode(
    endpoint: &ServiceEndpoint,
    scope: &[u8],
    position: Option<impl Serialize>,
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
        if position.review != review.as_str()
            || position.settings != settings
            || !(1..=max_limit).contains(&position.limit)
            || position.limit != limit
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
    discussion: &Discussion,
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
    let anchor = discussion.anchor.as_ref().map(project_anchor).transpose()?;
    let notes = discussion
        .notes
        .iter()
        .map(|row| dto::PatchDiscussionNote {
            note_id: row.id.to_string(),
            web_url: format!("{mr_web_url}#note_{}", row.id),
            author: row.author.clone(),
            author_id: row.author_id.and_then(reviewq_protocol::UpstreamIdDto::new),
            ours: ours(row.author_id),
            body: row.body.clone(),
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
        discussion_id: discussion.id.clone(),
        anchor,
        outdated: discussion.outdated,
        web_url,
        our_participation,
        resolution,
        notes,
    })
}

fn project_anchor(anchor: &DiscussionAnchor) -> Result<dto::PatchDiscussionAnchor, AppError> {
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
        old_path: anchor.old_path.clone(),
        new_path: anchor.new_path.clone(),
        old_line: anchor
            .old_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
        new_line: anchor
            .new_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
        range: anchor
            .range
            .as_ref()
            .map(|range| dto::PatchDiscussionRange {
                start: project_line(&range.start),
                end: project_line(&range.end),
            }),
    })
}

fn project_line(line: &DiscussionLine) -> dto::PatchDiscussionLine {
    dto::PatchDiscussionLine {
        old_line: line
            .old_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
        new_line: line
            .new_line
            .and_then(|line| SafeCountDto::new(u64::from(line))),
    }
}
