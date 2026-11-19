use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::changes::ApprovalFacts;
use reviewq_protocol::changes as dto;
use reviewq_protocol::{GitShaDto, ReviewIdDto, UpstreamIdDto, UtcTimestampDto};
use serde::Deserialize;

use crate::entry::pages::{count, parse, success};

use super::{ApiError, Entry, Path, Query, check_source, parse_review, read_source};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::entry) struct ApprovalQuery {
    source_sha: Option<GitShaDto>,
}

pub(in crate::entry) async fn approvals(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<ApprovalQuery>,
) -> Result<Json<dto::ApprovalViewResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    let source = read_source(&entry, &actor, &review_id).await?;
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let expected = query
        .source_sha
        .map(|value| parse(value.as_str()))
        .transpose()?;
    let current = provider
        .approvals(
            source.value.identity.clone(),
            expected,
            source.context.scope.additional_reviewers.clone(),
        )
        .await?;
    check_source(&entry, &actor, &review_id, &source).await?;
    let mut warnings = Vec::new();
    if current.approvals.is_none() {
        warnings.push(dto::ApprovalWarning::ApprovalsUnavailable);
    }
    if current.rules.is_none() {
        warnings.push(dto::ApprovalWarning::RulesUnavailable);
    }
    let rules = current
        .rules
        .map(|rows| {
            rows.into_iter()
                .map(|row| {
                    Ok(dto::ApprovalRule {
                        id: UpstreamIdDto::new(row.id).ok_or(AppError::Unavailable)?,
                        name: row.name,
                        approved: row.approved,
                        required: count(u64::from(row.required))?,
                    })
                })
                .collect::<Result<Vec<_>, AppError>>()
        })
        .transpose()?;
    Ok(Json(success(
        dto::ApprovalView {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            source_sha: GitShaDto::new(current.head.as_str()).ok_or(AppError::Internal)?,
            observed_at: timestamp(current.observed_at_ms)?,
            personal_approval: current.personal_approval,
            approvals: current.approvals.map(facts).transpose()?,
            rules,
            warnings,
        },
        source.cursor,
    )?))
}

fn facts(facts: ApprovalFacts) -> Result<dto::ApprovalFacts, AppError> {
    Ok(dto::ApprovalFacts {
        approved: facts.approved,
        required: facts
            .required
            .map(|value| count(u64::from(value)))
            .transpose()?,
        left: facts
            .left
            .map(|value| count(u64::from(value)))
            .transpose()?,
        approvers: facts
            .approvers
            .into_iter()
            .map(|person| {
                Ok(dto::Approver {
                    id: UpstreamIdDto::new(person.id).ok_or(AppError::Unavailable)?,
                    username: person.username,
                    approved_at: person.approved_at_ms.map(timestamp).transpose()?,
                })
            })
            .collect::<Result<_, AppError>>()?,
    })
}

fn timestamp(value: i64) -> Result<UtcTimestampDto, AppError> {
    UtcTimestampDto::from_unix_milliseconds(value).ok_or(AppError::Unavailable)
}
