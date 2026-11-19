use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::Response;
use futures_util::stream::try_unfold;
use reviewq_app::access::AppError;
use reviewq_app::reports::{
    self as app, ContextKey, ContextPosition, ContextRequest, Descriptor, MAX_REPORT_PAGE,
    ReportPosition,
};
use reviewq_domain::identity::{ReceiptId, Revision, RunId};
use reviewq_protocol::reports::{self as dto, ReportHashDto};
use reviewq_protocol::{
    GitShaDto, HistoryTurnIdDto, Page, PageCursorDto, ResourceIdDto, RevisionDto, Success,
    UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use crate::entry::error::{ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{count, parse, success};
use crate::entry::{ApiError, Entry};

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum View {
    #[default]
    Summary,
    Markdown,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ViewQuery {
    #[serde(default)]
    view: View,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct SectionQuery {
    receipt_id: Option<String>,
    offset: Option<u64>,
    characters: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DownloadQuery {
    download: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryQuery {
    cursor: Option<String>,
    limit: Option<u16>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Collection {
    #[default]
    Dispositions,
    Findings,
    Changes,
    Sections,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ContextQuery {
    cursor: Option<String>,
    limit: Option<u16>,
    receipt_id: Option<String>,
    #[serde(default)]
    collection: Collection,
}

#[derive(Deserialize, Serialize)]
struct HistoryCursor(i64, String);

#[derive(Deserialize, Serialize)]
struct ContextCursor {
    receipt: String,
    hash: String,
    revision: u64,
    key: CursorKey,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum CursorKey {
    Identity(String),
    Offset(u64),
}

const PAGE_BYTES: usize = 64 * 1024;

pub(super) async fn current(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    Query(query): Query<ViewQuery>,
) -> Result<Json<Success<dto::ReportView>>, ApiError> {
    view(entry, headers, parse(&run)?, None, query.view).await
}

pub(super) async fn version(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, receipt)): Path<(String, String)>,
    Query(query): Query<ViewQuery>,
) -> Result<Json<Success<dto::ReportView>>, ApiError> {
    view(
        entry,
        headers,
        parse(&run)?,
        Some(parse(&receipt)?),
        query.view,
    )
    .await
}

async fn view(
    entry: Arc<Entry>,
    headers: HeaderMap,
    run: RunId,
    receipt: Option<ReceiptId>,
    view: View,
) -> Result<Json<Success<dto::ReportView>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let stored = entry
        .store
        .read_report(actor, run, receipt, matches!(view, View::Markdown))
        .await?;
    Ok(Json(success(
        dto::ReportView {
            report: stored.value.report.map(summary).transpose()?,
            delivery: stored.value.delivery.map(delivery).transpose()?,
            markdown: stored.value.markdown,
        },
        stored.cursor,
    )?))
}

pub(super) async fn history(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let limit = page_limit(query.limit)?;
    let scope = format!("browser-report-history:{limit}:{run}");
    let endpoint = &entry.auth.endpoint;
    let after = query
        .cursor
        .as_deref()
        .map(|cursor| {
            let HistoryCursor(time, receipt) = endpoint.decode_cursor(scope.as_bytes(), cursor)?;
            Ok::<_, AppError>(ReportPosition {
                captured_at_ms: time,
                receipt_id: parse(&receipt)?,
            })
        })
        .transpose()?;
    let stored = entry
        .store
        .read_report_history(actor, parse(&run)?, after, limit)
        .await?;
    let data = stored.value;
    let current_receipt_id = data
        .current_receipt_id
        .map(|id| resource(id.as_str()))
        .transpose()?;
    let delivery = data.delivery.map(delivery).transpose()?;
    let items = data
        .items
        .into_iter()
        .map(|item| {
            let position =
                HistoryCursor(item.captured_at_ms, item.reference.receipt_id.to_string());
            Ok::<_, AppError>((summary(item)?, position))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(encode_page(
        &items,
        data.next.is_some(),
        stored.cursor,
        |position| cursor(endpoint.encode_cursor(scope.as_bytes(), position)?),
        |page| dto::ReportHistory {
            page,
            current_receipt_id: current_receipt_id.clone(),
            delivery: delivery.clone(),
        },
    )?)
}

pub(super) async fn context(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    Query(query): Query<ContextQuery>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let limit = page_limit(query.limit)?;
    let collection = match query.collection {
        Collection::Dispositions => app::Collection::Dispositions,
        Collection::Findings => app::Collection::Findings,
        Collection::Changes => app::Collection::Changes,
        Collection::Sections => app::Collection::Sections,
    };
    let scope = format!("browser-report-context:{collection:?}:{limit}:{run}");
    let endpoint = &entry.auth.endpoint;
    let after = query
        .cursor
        .as_deref()
        .map(|cursor| {
            let page: ContextCursor = endpoint.decode_cursor(scope.as_bytes(), cursor)?;
            Ok::<_, AppError>(ContextPosition {
                report: app::ReportReference {
                    receipt_id: parse(&page.receipt)?,
                    hash: parse(&page.hash)?,
                },
                disposition_version: Revision::try_from(page.revision)
                    .map_err(|_| AppError::CursorExpired)?,
                key: match page.key {
                    CursorKey::Identity(value) => ContextKey::Identity(value),
                    CursorKey::Offset(value) => ContextKey::Offset(value),
                },
            })
        })
        .transpose()?;
    let receipt = query.receipt_id.as_deref().map(parse).transpose()?;
    let stored = entry
        .store
        .read_report_context(
            actor,
            parse(&run)?,
            ContextRequest {
                receipt,
                collection,
                after,
                limit,
            },
        )
        .await?;
    let data = stored.value;
    let items = data
        .items
        .into_iter()
        .map(|item| {
            let key = match item.key() {
                ContextKey::Identity(value) => CursorKey::Identity(value),
                ContextKey::Offset(value) => CursorKey::Offset(value),
            };
            Ok::<_, AppError>((descriptor(item)?, key))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let base_report = data.report.as_ref().cloned().map(reference).transpose()?;
    let user_disposition_version =
        RevisionDto::new(data.disposition_version.get()).ok_or(AppError::Internal)?;
    Ok(encode_page(
        &items,
        data.next.is_some(),
        stored.cursor,
        |key| {
            let report = data.report.as_ref().ok_or(AppError::Internal)?;
            let position = ContextCursor {
                receipt: report.receipt_id.to_string(),
                hash: report.hash.to_string(),
                revision: data.disposition_version.get(),
                key: key.clone(),
            };
            cursor(endpoint.encode_cursor(scope.as_bytes(), &position)?)
        },
        |page| dto::ReportContext {
            page,
            base_report: base_report.clone(),
            user_disposition_version,
        },
    )?)
}

pub(super) async fn section(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    Query(query): Query<SectionQuery>,
) -> Result<Json<Success<dto::ReportSection>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let receipt = query.receipt_id.as_deref().map(parse).transpose()?;
    let stored = entry
        .store
        .read_report_section(
            actor,
            parse(&run)?,
            receipt,
            query.offset.unwrap_or(0),
            query.characters.unwrap_or(6000),
        )
        .await?;
    Ok(Json(success(
        project_section(stored.value)?,
        stored.cursor,
    )?))
}

pub(super) async fn candidate(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, receipt)): Path<(String, String)>,
    Query(query): Query<SectionQuery>,
) -> Result<Json<Success<dto::CandidateReport>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    if query.receipt_id.is_some() {
        return Err(AppError::InvalidRequest.into());
    }
    let stored = entry
        .store
        .read_report_candidate(
            actor,
            parse(&run)?,
            parse(&receipt)?,
            query.offset.unwrap_or(0),
            query.characters.unwrap_or(6000),
        )
        .await?;
    let candidate = stored.value;
    Ok(Json(success(
        dto::CandidateReport {
            accepted: false,
            section: project_section(candidate.section)?,
            stats: stats(candidate.stats)?,
            turn_id: HistoryTurnIdDto::new(candidate.turn_id).ok_or(AppError::Internal)?,
            call_id: ResourceIdDto::new(candidate.call_id).ok_or(AppError::Internal)?,
            declaration_sha256: hash(candidate.declaration_sha256.as_str())?,
            reason: candidate.reason,
        },
        stored.cursor,
    )?))
}

pub(super) async fn source(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, receipt)): Path<(String, String)>,
    Query(query): Query<DownloadQuery>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    if query.download != 1 {
        return Err(AppError::InvalidRequest.into());
    }
    let run: RunId = parse(&run)?;
    let receipt: ReceiptId = parse(&receipt)?;
    let stored = entry
        .store
        .read_report(actor.clone(), run.clone(), Some(receipt), false)
        .await?;
    let report = stored.value.report.ok_or(AppError::NotFound)?;
    let etag = format!("\"{}\"", report.reference.hash);
    if headers
        .get(header::IF_MATCH)
        .is_some_and(|value| value.as_bytes() != etag.as_bytes())
    {
        return Err(AppError::PreconditionFailed.into());
    }
    let len = report.stats.byte_length;
    let complete = report.stats.complete;
    let original_hash = report.original_sha256;
    let reference = report.reference;
    let receipt_value = reference.receipt_id.to_string();
    let body = Body::from_stream(try_unfold((0_u64, len), move |(ordinal, remaining)| {
        let (entry, actor, run, reference) =
            (entry.clone(), actor.clone(), run.clone(), reference.clone());
        async move {
            if remaining == 0 {
                return Ok(None);
            }
            let chunk = entry
                .store
                .read_report_chunk(actor, run, reference, ordinal)
                .await?
                .ok_or(AppError::Internal)?;
            let next = remaining
                .checked_sub(chunk.len() as u64)
                .ok_or(AppError::Internal)?;
            Ok::<_, AppError>(Some((chunk, (ordinal + 1, next))))
        }
    }));
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, "text/markdown; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            if complete {
                "attachment; filename=review-report.md"
            } else {
                "attachment; filename=review-report-retained.md"
            },
        )
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_LENGTH, len)
        .header(header::ETAG, etag)
        .header("x-reviewq-report-receipt", receipt_value)
        .header(
            "x-reviewq-report-complete",
            if complete { "true" } else { "false" },
        );
    if let Some(original_hash) = original_hash {
        response = response.header("x-reviewq-original-sha256", original_hash.as_str());
    }
    response.body(body).map_err(|_| AppError::Internal.into())
}

fn page_limit(limit: Option<u16>) -> Result<u16, AppError> {
    let limit = limit.unwrap_or(MAX_REPORT_PAGE);
    (1..=MAX_REPORT_PAGE)
        .contains(&limit)
        .then_some(limit)
        .ok_or(AppError::InvalidRequest)
}

fn encode_page<'a, T, K, D: Serialize>(
    items: &'a [(T, K)],
    has_more: bool,
    watermark: reviewq_domain::identity::EventCursor,
    encode_cursor: impl Fn(&K) -> Result<PageCursorDto, AppError>,
    project: impl Fn(Page<&'a T>) -> D,
) -> Result<Response, AppError> {
    let mut length = items.len();
    loop {
        let prefix = &items[..length];
        let next = if has_more || length < items.len() {
            Some(encode_cursor(
                &prefix.last().ok_or(AppError::PayloadTooLarge)?.1,
            )?)
        } else {
            None
        };
        let projection =
            Page::new(prefix.iter().map(|row| &row.0).collect(), next).ok_or(AppError::Internal)?;
        let body = serde_json::to_vec(&success(project(projection), watermark)?)
            .map_err(|_| AppError::Internal)?;
        if body.len() <= PAGE_BYTES {
            return Response::builder()
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .map_err(|_| AppError::Internal);
        }
        if length <= 1 {
            return Err(AppError::PayloadTooLarge);
        }
        length /= 2;
    }
}

fn cursor(value: String) -> Result<PageCursorDto, AppError> {
    PageCursorDto::new(value).ok_or(AppError::Internal)
}

fn hash(value: &str) -> Result<ReportHashDto, AppError> {
    ReportHashDto::new(value).ok_or(AppError::Internal)
}

fn resource(value: &str) -> Result<ResourceIdDto, AppError> {
    ResourceIdDto::new(value).ok_or(AppError::Internal)
}

fn sha(value: &reviewq_domain::identity::GitSha) -> Result<GitShaDto, AppError> {
    GitShaDto::new(value.as_str()).ok_or(AppError::Internal)
}

fn reference(value: app::ReportReference) -> Result<dto::ReportReference, AppError> {
    Ok(dto::ReportReference {
        receipt_id: resource(value.receipt_id.as_str())?,
        sha256: hash(value.hash.as_str())?,
    })
}

fn stats(value: app::ArtifactStats) -> Result<dto::ArtifactStats, AppError> {
    Ok(dto::ArtifactStats {
        byte_length: count(value.byte_length)?,
        total_code_points: count(value.total_code_points)?,
        line_count: count(value.line_count)?,
        max_line_bytes: count(value.max_line_bytes)?,
        complete: value.complete,
    })
}

fn project_section(value: app::ReportSection) -> Result<dto::ReportSection, AppError> {
    Ok(dto::ReportSection {
        report: reference(value.report)?,
        complete: value.complete,
        original_sha256: value
            .original_sha256
            .as_ref()
            .map(|value| hash(value.as_str()))
            .transpose()?,
        text: value.text,
        offset: count(value.offset)?,
        next_offset: value.next_offset.map(count).transpose()?,
        total_code_points: count(value.total_code_points)?,
        offset_unit: dto::OffsetUnit::UnicodeCodepoints,
    })
}

fn summary(value: app::ReportSummary) -> Result<dto::ReportSummary, AppError> {
    Ok(dto::ReportSummary {
        reference: reference(value.reference)?,
        original_sha256: value
            .original_sha256
            .as_ref()
            .map(|value| hash(value.as_str()))
            .transpose()?,
        native_provenance: value.native_provenance,
        parent_receipt_id: value
            .parent_receipt_id
            .map(|id| resource(id.as_str()))
            .transpose()?,
        attention_receipt_id: resource(value.attention_receipt_id.as_str())?,
        reviewed_sha: value.reviewed_sha.as_ref().map(sha).transpose()?,
        revision_reason: value.revision_reason,
        full_mr_reviewed_sha: value.full_mr_reviewed_sha.as_ref().map(sha).transpose()?,
        coverage: match value.coverage {
            app::Coverage::FullMr => dto::Coverage::FullMr,
            app::Coverage::SelectedThreads => dto::Coverage::SelectedThreads,
        },
        verdict: match value.verdict {
            app::Verdict::Approve => dto::Verdict::Approve,
            app::Verdict::RequestChanges => dto::Verdict::RequestChanges,
            app::Verdict::Blocked => dto::Verdict::Blocked,
            app::Verdict::NotSpecified => dto::Verdict::NotSpecified,
        },
        semantic_index: match value.semantic_index {
            app::SemanticIndex::Ready => dto::SemanticIndex::Ready,
            app::SemanticIndex::SourceOnly => dto::SemanticIndex::SourceOnly,
        },
        finding_count: value.finding_count.map(count).transpose()?,
        change_count: value.change_count.map(count).transpose()?,
        captured_at: UtcTimestampDto::from_unix_milliseconds(value.captured_at_ms)
            .ok_or(AppError::Internal)?,
        stats: stats(value.stats)?,
    })
}

fn delivery(value: app::ReportDelivery) -> Result<dto::ReportDelivery, AppError> {
    Ok(dto::ReportDelivery {
        receipt_id: resource(value.receipt_id.as_str())?,
        state: match value.state {
            app::ReportDeliveryState::Preparing => dto::DeliveryState::Preparing,
            app::ReportDeliveryState::Saving => dto::DeliveryState::Saving,
            app::ReportDeliveryState::Failed => dto::DeliveryState::Failed,
            app::ReportDeliveryState::Saved => dto::DeliveryState::Saved,
        },
        reason: value.reason,
    })
}

fn descriptor(value: Descriptor) -> Result<dto::Descriptor, AppError> {
    let range = |range: app::SourceRange| -> Result<dto::SourceRange, AppError> {
        Ok(dto::SourceRange {
            offset: count(range.offset)?,
            length: count(range.length)?,
            sha256: hash(range.hash.as_str())?,
        })
    };
    Ok(match value {
        Descriptor::Finding {
            id,
            title,
            severity,
            blocking,
            source,
        } => dto::Descriptor::Finding {
            finding_id: id,
            title,
            severity,
            blocking,
            source: source.map(range).transpose()?,
        },
        Descriptor::Section { title, source } => dto::Descriptor::Section {
            title,
            source: range(source)?,
        },
        Descriptor::Change {
            id,
            conclusion,
            reason,
            checked_sha,
        } => dto::Descriptor::Change {
            finding_id: id,
            reason,
            checked_sha: sha(&checked_sha)?,
            conclusion: match conclusion {
                app::FindingConclusion::Fixed => dto::FindingConclusion::Fixed,
                app::FindingConclusion::ClarifiedWithdrawn => {
                    dto::FindingConclusion::ClarifiedWithdrawn
                }
                app::FindingConclusion::Superseded => dto::FindingConclusion::Superseded,
                app::FindingConclusion::PartlyAddressed => dto::FindingConclusion::PartlyAddressed,
                app::FindingConclusion::StillPresent => dto::FindingConclusion::StillPresent,
                app::FindingConclusion::Unverified => dto::FindingConclusion::Unverified,
            },
        },
        Descriptor::Disposition {
            id,
            blocking,
            publication,
            reason,
        } => dto::Descriptor::Disposition {
            finding_id: id,
            blocking,
            reason,
            publication: publication.map(|publication| match publication {
                app::Publication::Withheld => dto::Publication::Withheld,
                app::Publication::Authorized => dto::Publication::Authorized,
                app::Publication::Published => dto::Publication::Published,
            }),
        },
    })
}
