use reviewq_app::access::{Actor, AppError};
use reviewq_app::hosts::{MessageBody, send_message};
use reviewq_app::queue::{ManagementFilter, RunSummary};
use reviewq_app::sources::ReviewResolution;
use reviewq_domain::identity::{ReviewId, RunId, SessionKey};
use reviewq_protocol::mcp::{
    GetReview, ListReviews, ReadTranscript, ReviewFilter, SendReviewMessage, TranscriptRead,
};
use serde_json::{Value, json};

use crate::entry::Entry;
use crate::entry::history::{ContentQuery, HistoryQuery, read, read_content};
use crate::entry::launch;
use crate::entry::messages;
use crate::entry::pages::{parse, project_review, queue_sync};

pub(super) enum ResolvedReview {
    Review(ReviewId),
    Alternatives(Vec<String>),
}

pub(super) async fn list(
    entry: &Entry,
    actor: Actor,
    args: ListReviews,
) -> Result<Value, AppError> {
    let limit = list_limit(args.limit)?;
    let offset = args.offset.unwrap_or(0);
    let filter = match args.filter.unwrap_or(ReviewFilter::NeedsInput) {
        ReviewFilter::All => ManagementFilter::All,
        ReviewFilter::NeedsInput => ManagementFilter::NeedsInput,
        ReviewFilter::AgentQuestions => ManagementFilter::AgentQuestions,
        ReviewFilter::Working => ManagementFilter::Working,
        ReviewFilter::Waiting => ManagementFilter::Waiting,
        ReviewFilter::Archived => ManagementFilter::Archived,
        ReviewFilter::Unstarted => ManagementFilter::Unstarted,
    };
    let page = entry
        .store
        .management_reviews(actor, filter, offset, limit)
        .await?
        .value;
    let mut values = Vec::with_capacity(page.items.len());
    for item in &page.items {
        let data = project_review(item)?;
        values.push(json!({
            "summary": data,
            "dashboard": dashboard(entry, &item.review_id, None),
        }));
    }
    let sync = queue_sync(entry.active_source().await?.collection_enabled, page.sync);
    Ok(json!({
        "lastSyncAt": sync.last_success_at,
        "lastSyncAttemptAt": sync.last_attempt_at,
        "syncFailed": sync.error.is_some(),
        "reviews": values, "total": page.total,
        "nextOffset": next_offset(offset, values.len(), page.total),
    }))
}

pub(super) async fn get(entry: &Entry, actor: Actor, args: GetReview) -> Result<Value, AppError> {
    let review = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id) => id,
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let limit = list_limit(args.limit)?;
    let offset = args.offset.unwrap_or(0);
    let launch = if let Some(id) = args.launch_submission_id {
        let stored = entry
            .store
            .launch_receipt(
                actor.clone(),
                Some(review.clone()),
                parse(id.as_str())?,
                None,
            )
            .await?
            .ok_or(AppError::NotFound)?;
        let stored = launch::reconcile(entry, actor.clone(), stored).await?;
        Some(launch::project_receipt(stored.value)?)
    } else {
        None
    };
    let selected = match args.session_key {
        Some(session) => Some(
            entry
                .store
                .run_for_session(actor.clone(), review.clone(), parse(session.as_str())?)
                .await?,
        ),
        None => None,
    };
    let page = entry
        .store
        .management_runs(actor, review.clone(), offset, limit)
        .await?
        .value;
    let runs: Vec<Value> = page
        .runs
        .iter()
        .map(|run| run_summary(entry, &review, run))
        .collect();
    let sync = queue_sync(entry.active_source().await?.collection_enabled, page.sync);
    Ok(json!({
        "review": project_review(&page.item)?,
        "runs":runs, "total":page.total,
        "nextOffset": next_offset(offset, runs.len(), page.total),
        "lastSyncAt":sync.last_success_at, "syncFailed":sync.error.is_some(),
        "dashboard":dashboard(entry, &review, selected.as_ref()),
        "launch": launch,
    }))
}

pub(super) fn run_summary(entry: &Entry, review: &ReviewId, run: &RunSummary) -> Value {
    let provider = match run.provider {
        reviewq_domain::session::HostProvider::Codex => "codex",
        reviewq_domain::session::HostProvider::Claude => "claude",
    };
    json!({
        "runId":run.run_id.as_str(),
        "leadSessionKey":run.lead_session_key.as_str(),
        "provider":provider,
        "startedAt":reviewq_protocol::UtcTimestampDto::from_unix_milliseconds(run.started_at_ms),
        "dashboard":dashboard(entry, review, Some(&run.run_id)),
    })
}

pub(super) fn alternatives(urls: Vec<String>) -> Value {
    json!({"state":"ambiguous", "choices": urls})
}

fn next_offset(offset: u32, size: usize, total: u64) -> Option<u32> {
    let next = offset.checked_add(u32::try_from(size).ok()?)?;
    (u64::from(next) < total).then_some(next)
}

pub(super) async fn transcript(
    entry: &Entry,
    actor: Actor,
    args: ReadTranscript,
) -> Result<Value, AppError> {
    let review = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id) => id,
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let (run, session) = select(
        entry,
        &actor,
        review,
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
                    around_id: None,
                    locator: None,
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
    let review = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id) => id,
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let (run, session) = if let Some(session) = &args.session_key {
        select(entry, &actor, review.clone(), Some(session.as_str())).await?
    } else {
        let choices = entry
            .store
            .management_runs(actor.clone(), review.clone(), 0, 10)
            .await?
            .value;
        if choices.total > 1 {
            let runs: Vec<_> = choices
                .runs
                .iter()
                .map(|run| run_summary(entry, &review, run))
                .collect();
            return Ok(json!({
                "state":"ambiguous", "choices":runs, "total":choices.total,
                "dashboard":dashboard(entry, &review, None),
            }));
        }
        let lead = choices.runs.into_iter().next().ok_or(AppError::NotFound)?;
        (lead.run_id, lead.lead_session_key)
    };
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
    id: ReviewId,
    session: Option<&str>,
) -> Result<(RunId, SessionKey), AppError> {
    if let Some(session) = session {
        let session: SessionKey = parse(session)?;
        let run = entry
            .store
            .run_for_session(actor.clone(), id.clone(), session.clone())
            .await?;
        return Ok((run, session));
    }
    let selected = entry
        .store
        .resolve_selection(actor.clone(), id.clone(), None)
        .await?
        .value;
    let lead = selected.lead().ok_or(AppError::NotFound)?;
    Ok((lead.run_id().clone(), lead.session_key().clone()))
}

pub(super) async fn resolve(
    entry: &Entry,
    actor: &Actor,
    target: &str,
) -> Result<ResolvedReview, AppError> {
    if let Ok(id) = parse::<ReviewId>(target) {
        match entry
            .store
            .resolve_selection(actor.clone(), id.clone(), None)
            .await
        {
            Ok(_) => return Ok(ResolvedReview::Review(id)),
            Err(AppError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    let source = entry
        .active_source()
        .await?
        .reader
        .ok_or(AppError::NotFound)?;
    let workspace = entry.store.workspace_context(actor.clone()).await?.value;
    match source
        .resolve(workspace.projects, target.to_owned())
        .await?
    {
        ReviewResolution::Ambiguous(urls) => Ok(ResolvedReview::Alternatives(urls)),
        ReviewResolution::Found(found) => entry
            .store
            .review_by_mr(actor.clone(), found.identity)
            .await
            .map(ResolvedReview::Review),
    }
}

pub(super) fn dashboard(entry: &Entry, review: &ReviewId, run: Option<&RunId>) -> String {
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
