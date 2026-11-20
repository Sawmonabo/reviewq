use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_protocol::changes as dto;
use reviewq_protocol::{EventCursorDto, RevisionDto, SafeCountDto, Success};

use super::{
    ApiError, ChangeQuery, Entry, Path, Position, Query, check_source, encode, file_metadata,
    parse_review, patch_revision, read_source,
};

const SELECTION_CURSOR: &[u8] = b"reviewq-changes-selection-v1";

pub(in crate::entry) async fn selected(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<ChangeQuery>,
) -> Result<Json<dto::SelectedPatchResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    if query.file_path_hint.is_some() == query.cursor.is_some()
        || query.revision.is_none()
        || query.preferred_file_id.is_some()
        || query.limit.is_some()
        || query.preview_edge.is_some()
        || query
            .file_path_hint
            .as_ref()
            .is_some_and(|hint| hint.is_empty() || hint.len() > 80)
    {
        return Err(AppError::InvalidRequest.into());
    }
    let hint = query.file_path_hint.clone();
    let source = read_source(&entry, &actor, &review_id).await?;
    let settings = source.context.revision.get();
    let (_, ordinal, revision) = query.position(
        &entry.auth.endpoint,
        SELECTION_CURSOR,
        &review_id,
        settings,
        1,
    )?;
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let result = provider
        .selected(
            source.value.identity.clone(),
            revision.ok_or(AppError::InvalidRequest)?,
            ordinal,
            hint,
        )
        .await?;
    check_source(&entry, &actor, &review_id, &source).await?;
    let cursor = |page: Option<u32>| {
        encode(
            &entry.auth.endpoint,
            SELECTION_CURSOR,
            page.map(|page| Position {
                review: review.clone(),
                settings,
                revision: result.revision.id.clone(),
                page,
                limit: 1,
            }),
        )
    };
    Ok(Json(Success {
        cursor: EventCursorDto::new(source.cursor.get()).ok_or(AppError::Internal)?,
        data: dto::SelectedPatch {
            settings_revision: RevisionDto::new(settings).ok_or(AppError::Internal)?,
            previous_cursor: cursor((result.ordinal > 1).then(|| result.ordinal - 1))?,
            next_cursor: cursor(result.next)?,
            ordinal: SafeCountDto::new(u64::from(result.ordinal)).ok_or(AppError::Internal)?,
            file: file_metadata(&review, &patch_revision(&result.revision.id)?, result.file)?,
            text: result.text,
        },
    }))
}
