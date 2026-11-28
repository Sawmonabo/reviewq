use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::Snapshot;
use reviewq_app::hosts::{DeliveryReceipt, MessageBody, MessageReplay, SubmitMessage};
use reviewq_app::queue::{ManagementFilter, RunSummary};
use reviewq_domain::identity::{ReviewId, RunId, SessionKey, SubmissionId};
use reviewq_protocol::mcp::{
    GetReview, ReadAgentTranscript, ReportRead, ReviewFilter, SearchReviews, SendAgentMessage,
    TranscriptRead,
};
use serde_json::{Value, json};

use crate::entry::Entry;
use crate::entry::features::{actions, followups, recovery, reports};
use crate::entry::history::{ContentQuery, HistoryQuery, read, read_content};
use crate::entry::launch;
use crate::entry::messages;
use crate::entry::pages::{PageQuery, parse, project_review, queue_sync, read_run};
use crate::entry::{questions, targets};

pub(super) use crate::entry::targets::{ResolvedReview, resolve};

pub(super) async fn list(
    entry: &Entry,
    actor: Actor,
    args: SearchReviews,
) -> Result<Value, AppError> {
    let limit = list_limit(args.limit)?;
    let offset = args.offset.unwrap_or(0);
    let search = args.query.unwrap_or_default();
    // A search looks at every stage unless the caller narrows it: only Awaiting input, the
    // listing's default, would hide most matches.
    let default = if search.is_empty() {
        ReviewFilter::NeedsInput
    } else {
        ReviewFilter::All
    };
    let filter = match args.filter.unwrap_or(default) {
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
        .management_reviews(actor, filter, search, offset, limit)
        .await?
        .value;
    let mut values = Vec::with_capacity(page.items.len());
    for item in &page.items {
        let data = project_review(item)?;
        let matched = page
            .findings
            .iter()
            .find(|found| found.review_id == item.review_id)
            .map(|found| {
                json!({
                    "findingId": found.finding_id,
                    "title": found.title,
                    "runId": found.run_id.as_str(),
                    "sessionKey": found.lead_session_key.as_str(),
                    "receiptId": found.receipt_id.as_str(),
                    "currentVersion": found.current,
                })
            });
        values.push(json!({
            "summary": data,
            "dashboard": dashboard(entry, &item.review_id, None),
            "matchedFinding": matched,
        }));
    }
    let active = entry.active_source().await?;
    if active.revision != page.settings {
        return Err(AppError::StaleRevision);
    }
    let sync = queue_sync(&active, page.sync);
    fit_page(&values, offset, page.total, |items, next| {
        json!({
            "lastSyncAt": sync.last_success_at,
            "lastSyncAttemptAt": sync.last_attempt_at,
            "syncFailed": sync.error.is_some(),
            "reviews": items, "total": page.total,
            "nextOffset": next,
        })
    })
}

pub(super) async fn get(entry: &Entry, actor: Actor, args: GetReview) -> Result<Value, AppError> {
    let (review, target_session) = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id, session) => (id, session),
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let session = targets::session(
        target_session,
        args.session_key.as_ref().map(|key| key.as_str()),
    )?;
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
    let selected = selection(entry, &actor, review.clone(), session).await?;
    let question_id = args.question_id.map(|id| id.as_str().to_owned());
    let (report, questions, action, sessions, recovery, delivery) = if let Some((run, session)) =
        &selected
    {
        let detail = read_run(
            entry,
            actor.clone(),
            run.clone(),
            PageQuery {
                limit: Some(list_limit(args.session_limit)?),
                cursor: args.session_cursor.map(|id| id.as_str().to_owned()),
            },
        )
        .await?
        .data;
        let lead: SessionKey = parse(detail.lead_session_key.as_str())?;
        let recovery = entry
            .store
            .recovery_state(actor.clone(), run.clone(), lead.clone())
            .await?
            .value
            .map(recovery::episode)
            .transpose()?;
        let delivery = if let Some(submission) = args.message_submission_id {
            let stored = reviewq_app::hosts::receipt(
                entry.store.as_ref(),
                actor.clone(),
                run.clone(),
                lead.clone(),
                parse(submission.as_str())?,
            )
            .await?;
            json!({"receipt":messages::project(stored)?.data})
        } else {
            let last = entry
                .store
                .read_messages(actor.clone(), run.clone(), lead, None, 1)
                .await?
                .value
                .items
                .into_iter()
                .next();
            last.map_or(Value::Null, |value| json!({
                "submissionId":value.submission_id.as_str(),
                "state": messages::recorded_state(value.state),
                "serviceAdmitted":value.service_admitted,
                "updatedAt":reviewq_protocol::UtcTimestampDto::from_unix_milliseconds(value.updated_at_ms),
                "outcomeDetail":value.outcome_detail,
            }))
        };
        let report = read_report(entry, actor.clone(), run.clone(), args.report).await?;
        let questions = questions::read(
            entry,
            actor.clone(),
            run.clone(),
            session.clone(),
            question_id,
        )
        .await?
        .data;
        let action = entry
            .store
            .read_action(actor.clone(), run.clone())
            .await?
            .value
            .map(actions::project)
            .transpose()?;
        (
            report,
            Some(questions),
            action,
            Some(detail.sessions),
            recovery,
            delivery,
        )
    } else {
        if args.report.is_some()
            || question_id.is_some()
            || args.session_cursor.is_some()
            || args.message_submission_id.is_some()
        {
            return Err(AppError::NotFound);
        }
        (Value::Null, None, None, None, None, Value::Null)
    };
    let followups = followups::project_enrollment(
        entry
            .store
            .read_followups(actor.clone(), review.clone())
            .await?
            .value,
    )?;
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
    let active = entry.active_source().await?;
    if active.revision != page.settings {
        return Err(AppError::StaleRevision);
    }
    let sync = queue_sync(&active, page.sync);
    let item = project_review(&page.item)?;
    fit_page(&runs, offset, page.total, |runs, next| {
        json!({
            "review": item,
            "runs":runs, "total":page.total,
            "nextOffset": next,
            "lastSyncAt":sync.last_success_at, "syncFailed":sync.error.is_some(),
            "dashboard":dashboard_session(entry,&review,selected.as_ref().map(|(run,_)|run),
                selected.as_ref().map(|(_,key)|key)),
            "selectedSessionKey": selected.as_ref().map(|(_, key)| key.as_str()),
            "launch": launch,
            "questions": questions,
            "report": report,
            "followups": followups,
            "action": action,
            "sessions": sessions,
            "delivery": delivery,
            "recovery": recovery,
        })
    })
}

async fn read_report(
    entry: &Entry,
    actor: Actor,
    run: RunId,
    report: Option<ReportRead>,
) -> Result<Value, AppError> {
    match report.unwrap_or(ReportRead::Summary { receipt_id: None }) {
        ReportRead::Summary { receipt_id } => {
            let receipt = receipt_id.map(|id| parse(id.as_str())).transpose()?;
            let stored = entry.store.read_report(actor, run, receipt, false).await?;
            Ok(json!({"kind":"summary","value":reports::project_view(stored.value)?}))
        }
        ReportRead::Section {
            receipt_id,
            offset,
            characters,
        } => {
            let receipt = receipt_id.map(|id| parse(id.as_str())).transpose()?;
            let stored = entry
                .store
                .read_report_section(
                    actor,
                    run,
                    receipt,
                    offset.map_or(0, |size| size.get()),
                    characters.unwrap_or(6000),
                )
                .await?;
            Ok(json!({"kind":"section","value":reports::project_section(stored.value)?}))
        }
        ReportRead::Versions { cursor, limit } => {
            super::lookups::versions(entry, actor, run, cursor, limit).await
        }
    }
}

pub(super) fn run_summary(entry: &Entry, review: &ReviewId, run: &RunSummary) -> Value {
    json!({
        "runId":run.run_id.as_str(),
        "leadSessionKey":run.lead_session_key.as_str(),
        "provider":provider_label(run.provider),
        "leadName":run.lead_name,
        "startedAt":reviewq_protocol::UtcTimestampDto::from_unix_milliseconds(run.started_at_ms),
        "dashboard":dashboard(entry, review, Some(&run.run_id)),
    })
}

fn provider_label(provider: reviewq_domain::session::HostProvider) -> &'static str {
    match provider {
        reviewq_domain::session::HostProvider::Codex => "codex",
        reviewq_domain::session::HostProvider::Claude => "claude",
    }
}

pub(super) fn alternatives(urls: Vec<String>) -> Value {
    json!({"state":"ambiguous", "choices": urls})
}

/// The longest page of at most `len` items whose result fits a tool result: `page(count)`
/// projects the first `count`, which is halved until the result fits.
pub(super) fn fit(
    len: usize,
    page: impl Fn(usize) -> Result<Value, AppError>,
) -> Result<Value, AppError> {
    let mut count = len;
    loop {
        let value = page(count)?;
        match super::bounded_result(&value) {
            Ok(_) => return Ok(value),
            Err(AppError::PayloadTooLarge) if count > 1 => count /= 2,
            Err(error) => return Err(error),
        }
    }
}

/// An offset page through `fit`: a shortened page continues after its last item.
fn fit_page(
    items: &[Value],
    offset: u32,
    total: u64,
    project: impl Fn(&[Value], Option<u32>) -> Value,
) -> Result<Value, AppError> {
    fit(items.len(), |count| {
        let next = offset.checked_add(count as u32);
        let next = next.filter(|position| u64::from(*position) < total);
        Ok(project(&items[..count], next))
    })
}

pub(super) async fn transcript(
    entry: &Entry,
    actor: Actor,
    args: ReadAgentTranscript,
) -> Result<Value, AppError> {
    let (review, target_session) = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id, session) => (id, session),
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let session = targets::session(
        target_session,
        args.session_key.as_ref().map(|key| key.as_str()),
    )?;
    let (run, session) = select(entry, &actor, review, session).await?;
    match args.read {
        TranscriptRead::Page {
            cursor,
            limit,
            direction,
            around_id,
            locator,
        } => {
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
                    around_id: around_id.map(|id| id.as_str().to_owned()),
                    locator,
                    direction,
                },
                budget,
                None,
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
        TranscriptRead::Search {
            query,
            cursor,
            direction,
        } => {
            let target = (run, session);
            super::lookups::search(entry, actor, target, &query, cursor.as_deref(), direction).await
        }
    }
}

pub(super) async fn send(
    entry: &Entry,
    actor: Actor,
    args: SendAgentMessage,
) -> Result<Value, AppError> {
    if !entry.sends_allowed {
        return Err(AppError::Forbidden);
    }
    let (review, target_session) = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id, session) => (id, session),
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let session = targets::session(
        target_session,
        args.session_key.as_ref().map(|key| key.as_str()),
    )?;
    let text = MessageBody::new(args.text.as_str())?;
    let submission: SubmissionId = parse(args.submission_id.as_str())?;
    let report = args
        .finding
        .map(|finding| {
            reports::feedback_reference(
                finding.receipt_id,
                finding.report_hash,
                &finding.quote,
                finding.quote_ref,
            )
        })
        .transpose()?;
    if let Some(receipt) = entry
        .store
        .replay_message(
            actor.clone(),
            MessageReplay {
                review: review.clone(),
                session: session.clone(),
                submission: submission.clone(),
                text: text.clone(),
                report: report.clone(),
            },
        )
        .await?
    {
        return send_result(entry, actor, receipt).await;
    }
    let (run, session) = if let Some(session) = session {
        select(entry, &actor, review.clone(), Some(session)).await?
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
    let lead = entry
        .store
        .lookup_lead(actor.clone(), run, session)
        .await?
        .value;
    let receipt = entry
        .store
        .admit_message(
            actor.clone(),
            SubmitMessage::user_input(lead, submission, text, report),
        )
        .await?;
    send_result(entry, actor, receipt).await
}

async fn send_result(
    entry: &Entry,
    actor: Actor,
    receipt: Snapshot<DeliveryReceipt>,
) -> Result<Value, AppError> {
    let target = receipt.value.target.clone();
    let companion = entry
        .store
        .claude_companion_available(actor, target.clone())
        .await?
        .map(|available| json!({"available": available}));
    Ok(json!({
        "receipt":messages::project(receipt)?.data,
        "dashboard":dashboard(entry, target.review_id(), Some(target.run_id())),
        "companion":companion,
    }))
}

/// The run a call on review `id` reads: the run of `session`, or the lead's, which a review
/// with no registered lead lacks.
async fn selection(
    entry: &Entry,
    actor: &Actor,
    id: ReviewId,
    session: Option<SessionKey>,
) -> Result<Option<(RunId, SessionKey)>, AppError> {
    if let Some(session) = session {
        let run = entry
            .store
            .run_for_session(actor.clone(), id, session.clone())
            .await?;
        return Ok(Some((run, session)));
    }
    Ok(entry
        .store
        .resolve_selection(actor.clone(), id, None)
        .await?
        .value
        .lead()
        .map(|lead| (lead.run_id().clone(), lead.session_key().clone())))
}

/// `selection`, for a call that needs a run.
pub(super) async fn select(
    entry: &Entry,
    actor: &Actor,
    id: ReviewId,
    session: Option<SessionKey>,
) -> Result<(RunId, SessionKey), AppError> {
    selection(entry, actor, id, session)
        .await?
        .ok_or(AppError::NotFound)
}

pub(super) fn dashboard(entry: &Entry, review: &ReviewId, run: Option<&RunId>) -> String {
    dashboard_session(entry, review, run, None)
}

fn dashboard_session(
    entry: &Entry,
    review: &ReviewId,
    run: Option<&RunId>,
    session: Option<&SessionKey>,
) -> String {
    let root = entry.auth.endpoint.browser();
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("view", if run.is_some() { "runs" } else { "queue" });
    query.append_pair("reviewId", review.as_str());
    if let Some(run) = run {
        query
            .append_pair("runId", run.as_str())
            .append_pair("tab", "agent");
    }
    if let Some(session) = session {
        query.append_pair("sessionKey", session.as_str());
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
