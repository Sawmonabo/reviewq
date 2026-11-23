use std::ops::Range;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::changes::{PatchAvailability, PatchContent, PatchRange, PatchRead};
use serde::Deserialize;

use super::{
    ApiError, Entry, Path, Query, check_source, parse_review, read_source, valid_revision,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::entry) struct RawQuery {
    revision: String,
    file_id: String,
    download: Option<u8>,
}

pub(in crate::entry) async fn patch(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<RawQuery>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    if query.file_id.is_empty()
        || query.file_id.len() > 80
        || !valid_revision(&query.revision)
        || query.download.is_some_and(|value| value != 1)
    {
        return Err(AppError::InvalidRequest.into());
    }
    let source = read_source(&entry, &actor, &review_id).await?;
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let read = if headers.contains_key(header::RANGE) {
        let mut ranges = headers.get_all(header::RANGE).iter();
        let range = ranges.next().and_then(|value| value.to_str().ok());
        PatchRead::Range(
            range
                .filter(|_| ranges.next().is_none())
                .and_then(byte_range),
        )
    } else {
        PatchRead::Full
    };
    let patch = provider
        .patch(
            source.value.identity.clone(),
            query.revision,
            query.file_id,
            read,
        )
        .await?;
    check_source(&entry, &actor, &review_id, &source).await?;
    if patch.file.availability != PatchAvailability::Complete {
        return Err(AppError::Unavailable.into());
    }
    let metrics = patch.file.metrics.ok_or(AppError::Internal)?;
    let etag = metrics.etag.as_str();
    if headers.contains_key(header::IF_MATCH)
        && !etag_match(&headers, header::IF_MATCH, etag, false)
    {
        return Err(AppError::PreconditionFailed.into());
    }
    let mut result = HeaderMap::new();
    put(&mut result, header::ETAG, etag)?;
    put(&mut result, header::ACCEPT_RANGES, "bytes")?;
    put(
        &mut result,
        header::CONTENT_TYPE,
        "text/plain; charset=utf-8",
    )?;
    put(
        &mut result,
        HeaderName::from_static("x-reviewq-review-id"),
        &review,
    )?;
    put(
        &mut result,
        HeaderName::from_static("x-reviewq-file-id"),
        &patch.file.id,
    )?;
    put(
        &mut result,
        HeaderName::from_static("x-reviewq-patch-revision"),
        &patch.revision.id,
    )?;
    if query.download == Some(1) {
        put(
            &mut result,
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"review.patch\"",
        )?;
    }
    if etag_match(&headers, header::IF_NONE_MATCH, etag, true) {
        return Ok((StatusCode::NOT_MODIFIED, result).into_response());
    }
    let length = metrics.byte_length;
    if let PatchRead::Range(range) = read {
        if let Some(requested) = range.and_then(|range| range.resolve(length)) {
            if !patch.body.covers(requested.start, requested.len()) {
                return Err(AppError::Unavailable.into());
            }
            put(
                &mut result,
                header::CONTENT_RANGE,
                &format!("bytes {}-{}/{length}", requested.start, requested.end - 1),
            )?;
            put(
                &mut result,
                header::CONTENT_LENGTH,
                &requested.len().to_string(),
            )?;
            return Ok((
                StatusCode::PARTIAL_CONTENT,
                result,
                stream(patch.body, requested),
            )
                .into_response());
        }
        let mut error = ApiError::from(AppError::RangeNotSatisfiable).into_response();
        put(
            error.headers_mut(),
            header::CONTENT_RANGE,
            &format!("bytes */{length}"),
        )?;
        return Ok(error);
    }
    if !patch.body.covers(0, length) {
        return Err(AppError::Unavailable.into());
    }
    put(&mut result, header::CONTENT_LENGTH, &length.to_string())?;
    Ok((result, stream(patch.body, 0..length)).into_response())
}

fn stream(body: Arc<dyn PatchContent>, range: Range<usize>) -> Body {
    Body::from_stream(futures_util::stream::try_unfold(
        (body, range),
        |(body, range)| async move {
            if range.is_empty() {
                return Ok::<_, AppError>(None);
            }
            let length = range.len().min(64 * 1024);
            let bytes = body.read(range.start, length).await?;
            Ok(Some((
                Bytes::from(bytes),
                (body, range.start + length..range.end),
            )))
        },
    ))
}

fn etag_match(headers: &HeaderMap, name: HeaderName, etag: &str, weak: bool) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|value| {
            let item = value.trim();
            let compare = if weak {
                item.strip_prefix("W/").unwrap_or(item)
            } else {
                item
            };
            compare == "*" || compare == etag
        })
}

fn byte_range(value: &str) -> Option<PatchRange> {
    let (first, last) = value.strip_prefix("bytes=")?.split_once('-')?;
    let integer = |raw: &str| -> Option<usize> {
        if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        raw.parse().ok()
    };
    if first.is_empty() {
        return Some(PatchRange::Suffix(integer(last)?));
    }
    let start = integer(first)?;
    let last = if last.is_empty() {
        None
    } else {
        Some(integer(last)?)
    };
    Some(PatchRange::From { start, last })
}

fn put(headers: &mut HeaderMap, name: HeaderName, value: &str) -> Result<(), AppError> {
    let value = HeaderValue::from_str(value).map_err(|_| AppError::Internal)?;
    headers.insert(name, value);
    Ok(())
}
