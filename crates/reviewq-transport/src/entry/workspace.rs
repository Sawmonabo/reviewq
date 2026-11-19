use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::queue::{MAX_QUEUE_PAGE, RunCardsFilter, RunCardsPosition};
use reviewq_domain::review_stage::RunStage;
use reviewq_protocol::queue as dto;
use reviewq_protocol::{Page, PageCursorDto, RunIdDto};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::ApiQuery as Query;
use super::pages::{Context, count, parse, project_review, success};
use super::{ApiError, Entry};

const RUNS: &[u8] = b"reviewq-run-cards-v1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RunQuery {
    stage: Option<dto::RunStage>,
    reason_kind: Option<String>,
    has_agent_questions: Option<bool>,
    q: Option<String>,
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct RunPosition {
    context: Context,
    filter: [u8; 32],
    activity: i64,
    review: String,
}

pub(super) async fn runs(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Query(query): Query<RunQuery>,
) -> Result<Json<dto::RunCardsResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let stage = query.stage.map(|stage| match stage {
        dto::RunStage::NeedsInput => RunStage::NeedsInput,
        dto::RunStage::Working => RunStage::Working,
        dto::RunStage::Waiting => RunStage::Waiting,
        dto::RunStage::Archived => RunStage::Archived,
    });
    let search = query.q.unwrap_or_default();
    if search.chars().count() > 200 {
        return Err(AppError::InvalidRequest.into());
    }
    let limit = query.limit.unwrap_or(50);
    if !(1..=MAX_QUEUE_PAGE).contains(&limit) {
        return Err(AppError::InvalidRequest.into());
    }
    let filter = signature(&(
        stage.map(RunStage::as_str),
        query.reason_kind.as_deref(),
        query.has_agent_questions,
        &search,
    ))?;
    let endpoint = &entry.auth.endpoint;
    let after = query
        .cursor
        .as_deref()
        .map(|token| {
            let position: RunPosition = endpoint.decode_cursor(RUNS, token)?;
            if position.filter != filter {
                return Err(AppError::CursorExpired);
            }
            Ok(RunCardsPosition {
                context: position.context.try_into()?,
                activity: position.activity,
                review: parse(&position.review)?,
            })
        })
        .transpose()?;
    let page = entry
        .store
        .run_cards(
            actor,
            RunCardsFilter {
                stage,
                reason: query.reason_kind,
                has_questions: query.has_agent_questions,
                search,
            },
            after,
            limit,
        )
        .await?;
    let next = page
        .value
        .next
        .map(|position| {
            let token = endpoint.encode_cursor(
                RUNS,
                &RunPosition {
                    context: position.context.into(),
                    filter,
                    activity: position.activity,
                    review: position.review.to_string(),
                },
            )?;
            PageCursorDto::new(token).ok_or(AppError::Internal)
        })
        .transpose()?;
    let items = page
        .value
        .items
        .into_iter()
        .map(|item| {
            Ok(dto::RunCard {
                review: project_review(&item.review)?,
                run_ids: item
                    .runs
                    .into_iter()
                    .map(|id| RunIdDto::new(id.as_str()).ok_or(AppError::Internal))
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let counts = page.value.counts;
    Ok(Json(success(
        dto::RunCards {
            page: Page::new(items, next).ok_or(AppError::Internal)?,
            stage_counts: dto::StageCounts {
                needs_input: count(counts.needs_input)?,
                working: count(counts.working)?,
                waiting: count(counts.waiting)?,
                archived: count(counts.archived)?,
            },
            total_matching: count(page.value.total)?,
        },
        page.cursor,
    )?))
}

fn signature(value: &impl Serialize) -> Result<[u8; 32], AppError> {
    let bytes = serde_json::to_vec(value).map_err(|_| AppError::Internal)?;
    Ok(Sha256::digest(bytes).into())
}
