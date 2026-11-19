use reviewq_app::access::{Actor, AppError};
use reviewq_app::hosts::{MessageBody, send_message};
use reviewq_app::queue::{MergeRequestState, QueueItem};
use reviewq_app::sources::ReviewResolution;
use reviewq_domain::identity::{ReviewId, RunId, SessionKey};
use reviewq_protocol::mcp::{
    GetReview, ListReviews, ReadTranscript, ReviewFilter, SendReviewMessage, TranscriptRead,
};
use serde_json::{Value, json};

use crate::entry::Entry;
use crate::entry::history::{ContentQuery, HistoryQuery, read, read_content};
use crate::entry::messages;
use crate::entry::pages::{parse, project_review, queue_sync};

const MAX_OFFSET: u32 = 4096;

pub(super) async fn list(
    entry: &Entry,
    actor: Actor,
    args: ListReviews,
) -> Result<Value, AppError> {
    let limit = list_limit(args.limit)?;
    let offset = offset(args.offset)?;
    let filter = args.filter.unwrap_or(ReviewFilter::All);
    if matches!(
        filter,
        ReviewFilter::NeedsInput
            | ReviewFilter::AgentQuestions
            | ReviewFilter::Working
            | ReviewFilter::Waiting
    ) {
        return Err(AppError::Unavailable);
    }
    let mut next = None;
    let mut total = 0_u64;
    let mut values = Vec::new();
    let sync = loop {
        let rows = entry
            .store
            .read_queue(actor.clone(), next, 100)
            .await?
            .value;
        if rows.total > u64::from(MAX_OFFSET) && !matches!(filter, ReviewFilter::All) {
            return Err(AppError::PayloadTooLarge);
        }
        for item in rows.items.iter().filter(|row| matches_review(row, filter)) {
            if total >= u64::from(offset) && values.len() < usize::from(limit) {
                let data = project_review(item)?;
                values.push(json!({
                    "summary": data,
                    "dashboard": dashboard(entry, &item.review_id, None)
                }));
            }
            total += 1;
        }
        let sync = queue_sync(entry.source.is_some(), rows.sync);
        if matches!(filter, ReviewFilter::All) && values.len() == usize::from(limit) {
            total = rows.total;
            break sync;
        }
        next = rows.next;
        if next.is_none() {
            break sync;
        }
    };
    let next = u64::from(offset) + values.len() as u64;
    Ok(json!({
        "lastSyncAt": sync.last_success_at,
        "lastSyncAttemptAt": sync.last_attempt_at,
        "syncFailed": sync.error.is_some(),
        "reviews": values,
        "total": total,
        "nextOffset": (next < total).then_some(next)
    }))
}

fn matches_review(row: &QueueItem, filter: ReviewFilter) -> bool {
    match filter {
        ReviewFilter::All => true,
        ReviewFilter::Unstarted => {
            row.registered_lead_count == 0 && row.source.state == MergeRequestState::Open
        }
        ReviewFilter::Archived => {
            row.registered_lead_count != 0 && row.source.state != MergeRequestState::Open
        }
        _ => false,
    }
}

pub(super) async fn get(entry: &Entry, actor: Actor, args: GetReview) -> Result<Value, AppError> {
    let review = resolve(entry, &actor, &args.target).await?;
    let limit = list_limit(args.limit)?;
    let offset = offset(args.offset)? as usize;
    let selected = match args.session_key {
        Some(session) => {
            let session: SessionKey = parse(session.as_str())?;
            Some(
                entry
                    .store
                    .run_for_session(actor.clone(), review.clone(), session)
                    .await?,
            )
        }
        None => None,
    };
    let mut next = None;
    let mut total = 0;
    let mut result = Vec::new();
    let summary = loop {
        let overview = entry
            .store
            .read_review(actor.clone(), review.clone(), next, 100)
            .await?
            .value;
        let remaining = usize::from(limit).saturating_sub(result.len());
        let index = offset.saturating_sub(total);
        total += overview.runs.len();
        if total > MAX_OFFSET as usize {
            return Err(AppError::PayloadTooLarge);
        }
        for run in overview.runs.into_iter().skip(index).take(remaining) {
            let provider = match run.provider {
                reviewq_domain::session::HostProvider::Codex => "codex",
                reviewq_domain::session::HostProvider::Claude => "claude",
            };
            result.push(json!({
                "runId":run.run_id.as_str(),
                "leadSessionKey":run.lead_session_key.as_str(),
                "provider":provider,
                "startedAt":reviewq_protocol::UtcTimestampDto::from_unix_milliseconds(run.started_at_ms),
                "dashboard":dashboard(entry, &review, Some(&run.run_id)),
            }));
        }
        next = overview.next_run;
        if next.is_none() {
            break project_review(&overview.item)?;
        }
    };
    let after = offset + result.len();
    let sync = entry.store.read_queue(actor, None, 1).await?.value.sync;
    let sync = queue_sync(entry.source.is_some(), sync);
    Ok(json!({
        "review":summary, "runs":result, "total":total,
        "nextOffset":(after < total).then_some(after),
        "lastSyncAt":sync.last_success_at, "syncFailed":sync.error.is_some(),
        "dashboard":dashboard(entry, &review, selected.as_ref())
    }))
}

pub(super) async fn transcript(
    entry: &Entry,
    actor: Actor,
    args: ReadTranscript,
) -> Result<Value, AppError> {
    let (_, run, session) = select(
        entry,
        &actor,
        &args.target,
        args.session_key.as_ref().map(|id| id.as_str()),
    )
    .await?;
    match args.read {
        TranscriptRead::Page { cursor, limit } => {
            let budget = if limit.is_none() {
                16 * 1024
            } else {
                32 * 1024
            };
            let limit = limit.unwrap_or(10);
            if !(1..=25).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            let encoded = read(
                entry,
                actor,
                run,
                session,
                HistoryQuery {
                    limit: Some(limit),
                    cursor,
                },
                budget,
            )
            .await?;
            let mut value: Value =
                serde_json::from_slice(&encoded).map_err(|_| AppError::Internal)?;
            Ok(value.get_mut("data").ok_or(AppError::Internal)?.take())
        }
        TranscriptRead::Content { reference, cursor } => {
            let source = read_content(
                entry,
                actor,
                run,
                session,
                ContentQuery { reference, cursor },
                8 * 1024,
            )
            .await?;
            serde_json::to_value(source.data).map_err(|_| AppError::Internal)
        }
    }
}

pub(super) async fn send(
    entry: &Entry,
    actor: Actor,
    args: SendReviewMessage,
) -> Result<Value, AppError> {
    if !entry.sends_allowed {
        return Err(AppError::Forbidden);
    }
    let (review, run, session) = select(
        entry,
        &actor,
        &args.target,
        args.session_key.as_ref().map(|id| id.as_str()),
    )
    .await?;
    let message = MessageBody::new(args.text.as_str())?;
    let receipt = send_message(
        entry.store.as_ref(),
        actor,
        run.clone(),
        session,
        parse(args.submission_id.as_str())?,
        message,
    )
    .await?;
    Ok(json!({
        "receipt":messages::project(receipt)?.data,
        "dashboard":dashboard(entry, &review, Some(&run))
    }))
}

async fn select(
    entry: &Entry,
    actor: &Actor,
    target: &str,
    session: Option<&str>,
) -> Result<(ReviewId, RunId, SessionKey), AppError> {
    let id = resolve(entry, actor, target).await?;
    if let Some(session) = session {
        let session: SessionKey = parse(session)?;
        let run = entry
            .store
            .run_for_session(actor.clone(), id.clone(), session.clone())
            .await?;
        return Ok((id, run, session));
    }
    let selected = entry
        .store
        .resolve_selection(actor.clone(), id.clone(), None)
        .await?
        .value;
    let lead = selected.lead().ok_or(AppError::NotFound)?;
    Ok((id, lead.run_id().clone(), lead.session_key().clone()))
}

async fn resolve(entry: &Entry, actor: &Actor, target: &str) -> Result<ReviewId, AppError> {
    if let Ok(id) = parse::<ReviewId>(target) {
        match entry
            .store
            .resolve_selection(actor.clone(), id.clone(), None)
            .await
        {
            Ok(_) => return Ok(id),
            Err(AppError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    let source = entry.source.as_ref().ok_or(AppError::NotFound)?;
    let workspace = entry.store.workspace_context(actor.clone()).await?.value;
    let result = source
        .resolve(workspace.projects, target.to_owned())
        .await?;
    let ReviewResolution::Found(found) = result else {
        return Err(AppError::Conflict);
    };
    entry
        .store
        .review_by_mr(actor.clone(), found.identity)
        .await
}

fn dashboard(entry: &Entry, review: &ReviewId, run: Option<&RunId>) -> String {
    let root = entry.auth.endpoint.browser();
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("view", if run.is_some() { "runs" } else { "queue" });
    query.append_pair("reviewId", review.as_str());
    if let Some(run) = run {
        query
            .append_pair("runId", run.as_str())
            .append_pair("tab", "agent");
    }
    format!("http://{root}/?{}", query.finish())
}

fn list_limit(value: Option<u16>) -> Result<u16, AppError> {
    let value = value.unwrap_or(25);
    (1..=25)
        .contains(&value)
        .then_some(value)
        .ok_or(AppError::InvalidRequest)
}

fn offset(value: Option<u32>) -> Result<u32, AppError> {
    let value = value.unwrap_or(0);
    (value <= MAX_OFFSET)
        .then_some(value)
        .ok_or(AppError::InvalidRequest)
}
