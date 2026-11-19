use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::hosts::{QuestionAccess, QuestionAnswer, QuestionForm, QuestionStatus};
use reviewq_domain::identity::{RunId, SessionKey};
use reviewq_domain::session::HostProvider;
use reviewq_protocol::questions as dto;
use reviewq_protocol::{
    HistoryEntryIdDto, HistoryTurnIdDto, ResourceIdDto, RunIdDto, SessionKeyDto,
};
use serde::Deserialize;

use super::error::{ApiBytes, ApiPath as Path, ApiQuery as Query};
use super::pages::{parse, success};
use super::{ApiError, Entry};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Address {
    request_id: Option<String>,
}

pub(super) async fn list(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    Query(query): Query<Address>,
) -> Result<Json<dto::FormsResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    Ok(Json(
        read(
            &entry,
            actor,
            parse(&run)?,
            parse(&session)?,
            query.request_id,
        )
        .await?,
    ))
}

pub(super) async fn read(
    entry: &Entry,
    actor: Actor,
    run: RunId,
    session: SessionKey,
    request: Option<String>,
) -> Result<dto::FormsResponse, AppError> {
    validate_request(request.as_deref())?;
    let stored = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let host = entry
        .host
        .as_ref()
        .filter(|_| stored.value.binding.identity().provider() == HostProvider::Codex);
    let (available, forms) = match host {
        Some(host) => (
            true,
            host.questions(
                QuestionAccess::new(actor, stored.value, entry.store.clone()),
                request.clone(),
            )
            .await?,
        ),
        None => (false, Vec::new()),
    };
    if available && request.is_some() && forms.is_empty() {
        return Err(AppError::NotFound);
    }
    let items = forms
        .into_iter()
        .map(|form| project(&run, &session, form))
        .collect::<Result<_, _>>()?;
    success(
        dto::Forms {
            run_id: run_id(&run)?,
            session_key: session_key(&session)?,
            available,
            items,
        },
        stored.cursor,
    )
}

pub(super) async fn answer(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session, request)): Path<(String, String, String)>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::AnswerResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input =
        serde_json::from_slice::<dto::Answer>(&bytes).map_err(|_| AppError::InvalidRequest)?;
    Ok(Json(
        respond(
            &entry,
            actor,
            parse(&run)?,
            parse(&session)?,
            request,
            input,
        )
        .await?,
    ))
}

pub(super) async fn respond(
    entry: &Entry,
    actor: Actor,
    run: RunId,
    session: SessionKey,
    request: String,
    input: dto::Answer,
) -> Result<dto::AnswerResponse, AppError> {
    if !entry.sends_allowed {
        return Err(AppError::Forbidden);
    }
    validate_request(Some(&request))?;
    let stored = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    if stored.value.binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::Unavailable);
    }
    let host = entry.host.as_ref().ok_or(AppError::Unavailable)?;
    let submission = parse(input.submission_id.as_str())?;
    let answers = input
        .answers
        .into_iter()
        .map(|(id, value)| (id, value.answers))
        .collect();
    let answer = QuestionAnswer::new(
        request,
        submission,
        answers,
        QuestionAccess::new(actor, stored.value, entry.store.clone()),
    );
    let form = host.answer_question(answer).await?;
    success(project(&run, &session, form)?, stored.cursor)
}

fn project(run: &RunId, session: &SessionKey, value: QuestionForm) -> Result<dto::Form, AppError> {
    Ok(dto::Form {
        run_id: run_id(run)?,
        session_key: session_key(session)?,
        request_id: ResourceIdDto::new(value.request).ok_or(AppError::Internal)?,
        turn_id: HistoryTurnIdDto::new(value.turn.as_str()).ok_or(AppError::Internal)?,
        item_id: HistoryEntryIdDto::new(value.item.as_str()).ok_or(AppError::Internal)?,
        blocking: value.blocking,
        status: match value.state {
            QuestionStatus::Pending => dto::Status::Pending,
            QuestionStatus::Submitting => dto::Status::Submitting,
            QuestionStatus::Answered => dto::Status::Answered,
            QuestionStatus::Closed => dto::Status::Closed,
            QuestionStatus::Unavailable => dto::Status::Unavailable,
            QuestionStatus::Uncertain => dto::Status::Uncertain,
        },
        questions: value
            .questions
            .into_iter()
            .map(|question| dto::Question {
                id: question.id,
                header: question.header,
                question: question.prompt,
                options: question
                    .options
                    .into_iter()
                    .map(|option| dto::Choice {
                        label: option.label,
                        description: option.description,
                    })
                    .collect(),
                is_other: question.allows_other,
                is_secret: question.secret,
            })
            .collect(),
    })
}

fn run_id(run: &RunId) -> Result<RunIdDto, AppError> {
    RunIdDto::new(run.as_str()).ok_or(AppError::Internal)
}

fn session_key(session: &SessionKey) -> Result<SessionKeyDto, AppError> {
    SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)
}

fn validate_request(request: Option<&str>) -> Result<(), AppError> {
    if request.is_some_and(|value| ResourceIdDto::new(value).is_none()) {
        return Err(AppError::InvalidRequest);
    }
    Ok(())
}
