use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, ActorScope, AppError};
use reviewq_app::history::HistoryBookmark;
use reviewq_app::hosts::{
    ObservedAsyncQuestion, Question, QuestionAccess, QuestionAnswer, QuestionForm, QuestionMode,
    QuestionStatus, QuestionsPort,
};
use reviewq_domain::identity::{NativeInputId, NativeTurnId, RunId, SessionKey, SubmissionId};
use reviewq_domain::session::{HostProvider, NativeIdentity};
use reviewq_protocol::questions as dto;
use reviewq_protocol::{
    HistoryEntryIdDto, HistoryTurnIdDto, ResourceIdDto, RunIdDto, SessionKeyDto,
};
use serde::Deserialize;

use super::error::{ApiBytes, ApiPath as Path, ApiQuery as Query};
use super::pages::{parse, success};
use super::{ApiError, Entry, ServiceEndpoint, history};

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
    let host = question_port(entry, stored.value.binding.identity());
    let mut forms = match host {
        Some(host) => {
            host.questions(
                QuestionAccess::new(actor.clone(), stored.value.clone(), entry.store.clone()),
                request.clone(),
            )
            .await?
        }
        None if stored.value.binding.identity().provider() == HostProvider::Codex => entry
            .store
            .question_attempts(actor.clone(), stored.value.clone(), request.clone())
            .await?
            .into_iter()
            .map(|receipt| receipt.form())
            .collect(),
        None => Vec::new(),
    };
    if stored.value.binding.identity().provider() == HostProvider::Codex {
        let known = entry
            .store
            .observed_async_questions(actor.clone(), stored.value.clone(), request.clone())
            .await?;
        let observed = match entry
            .file_history
            .async_questions(stored.value.clone(), known, request.clone())
            .await
        {
            Ok(observed) => observed,
            Err(_) if !forms.is_empty() => Vec::new(),
            Err(error) => return Err(error),
        };
        for form in observed {
            if let Some(prior) = forms.iter_mut().find(|prior| prior.request == form.request) {
                *prior = form;
            } else {
                forms.push(form);
            }
        }
        forms.sort_by_key(|form| {
            !matches!(
                form.state,
                QuestionStatus::Pending | QuestionStatus::Submitting
            )
        });
        forms.truncate(8);
    }
    let local_operator = matches!(actor.scope(), ActorScope::LocalOperator);
    let can_answer = entry.sends_allowed
        && host.is_some()
        && (stored.value.binding.identity().provider() == HostProvider::Codex || local_operator);
    if entry
        .store
        .history_session(actor, run.clone(), session.clone())
        .await?
        .value
        != stored.value
    {
        return Err(AppError::StaleRevision);
    }
    let available = host.is_some() || !forms.is_empty();
    if available && request.is_some() && forms.is_empty() {
        return Err(AppError::NotFound);
    }
    let items = forms
        .into_iter()
        .map(|form| {
            project(
                &entry.auth.endpoint,
                &run,
                &session,
                form,
                can_answer,
                local_operator,
            )
        })
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
            Submission {
                request,
                id: parse(input.submission_id.as_str())?,
                answers: input.answers,
                address: Some((
                    parse(input.turn_id.as_str())?,
                    parse(input.item_id.as_str())?,
                )),
            },
        )
        .await?,
    ))
}

pub(super) struct Submission {
    pub request: String,
    pub id: SubmissionId,
    pub answers: BTreeMap<String, dto::Values>,
    pub address: Option<(NativeTurnId, NativeInputId)>,
}

pub(super) async fn respond(
    entry: &Entry,
    actor: Actor,
    run: RunId,
    session: SessionKey,
    input: Submission,
) -> Result<dto::AnswerResponse, AppError> {
    if !entry.sends_allowed {
        return Err(AppError::Forbidden);
    }
    validate_request(Some(&input.request))?;
    let stored = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let identity = stored.value.binding.identity();
    let known = if identity.provider() == HostProvider::Codex {
        entry
            .store
            .observed_async_questions(
                actor.clone(),
                stored.value.clone(),
                Some(input.request.clone()),
            )
            .await?
            .pop()
    } else {
        if input.address.is_none() || !matches!(actor.scope(), ActorScope::LocalOperator) {
            return Err(AppError::Forbidden);
        }
        None
    };
    let host = question_port(entry, identity).ok_or(AppError::Unavailable)?;
    let answers = input
        .answers
        .into_iter()
        .map(|(id, value)| (id, value.answers))
        .collect();
    let local_operator = matches!(actor.scope(), ActorScope::LocalOperator);
    let answer = QuestionAnswer::new(
        input.request,
        input.id,
        answers,
        QuestionAccess::new(actor.clone(), stored.value, entry.store.clone()),
        input.address,
    );
    let form = match known {
        Some(evidence) => respond_async(entry, actor, answer, evidence).await?,
        None => host.answer_question(answer).await?,
    };
    success(
        project(
            &entry.auth.endpoint,
            &run,
            &session,
            form,
            true,
            local_operator,
        )?,
        stored.cursor,
    )
}

async fn respond_async(
    entry: &Entry,
    actor: Actor,
    answer: QuestionAnswer,
    evidence: ObservedAsyncQuestion,
) -> Result<QuestionForm, AppError> {
    answer.check_address(
        &evidence.key.request,
        &evidence.key.turn,
        &evidence.key.item,
    )?;
    let digest = answer.digest()?;
    let session = answer.session().clone();
    if let Some(state) = entry
        .store
        .replay_async_answer(
            actor.clone(),
            session.clone(),
            answer.submission.clone(),
            answer.request.clone(),
            digest,
        )
        .await?
    {
        let bare = evidence.form(state);
        let form = entry
            .file_history
            .async_questions(session, vec![evidence], Some(answer.request))
            .await
            .ok()
            .and_then(|mut forms| forms.pop())
            .unwrap_or(bare);
        return Ok(QuestionForm { state, ..form });
    }
    let mut form = entry
        .file_history
        .prove_async_question(session.clone(), evidence.clone())
        .await?
        .ok_or(AppError::Unavailable)?;
    if form.state != QuestionStatus::Pending {
        return Err(AppError::PreconditionFailed);
    }
    let body = answer.async_body(&form)?;
    form.state = entry
        .store
        .admit_async_answer(actor, session, answer.submission, evidence, body, digest)
        .await?;
    Ok(form)
}

fn project(
    endpoint: &ServiceEndpoint,
    run: &RunId,
    session: &SessionKey,
    value: QuestionForm,
    can_answer: bool,
    local_operator: bool,
) -> Result<dto::Form, AppError> {
    let locator = value
        .locator
        .map(|locator| {
            history::entry_locator(
                endpoint,
                run,
                session,
                &locator.generation,
                &value.item,
                &HistoryBookmark::File(locator.file),
                Some(&value.turn),
            )
        })
        .transpose()?;
    Ok(dto::Form {
        run_id: run_id(run)?,
        session_key: session_key(session)?,
        request_id: ResourceIdDto::new(value.request).ok_or(AppError::Internal)?,
        turn_id: HistoryTurnIdDto::new(value.turn.as_str()).ok_or(AppError::Internal)?,
        item_id: HistoryEntryIdDto::new(value.item.as_str()).ok_or(AppError::Internal)?,
        blocking: value.blocking,
        mode: match value.mode {
            QuestionMode::Native => dto::Mode::Native,
            QuestionMode::Async => dto::Mode::Async,
        },
        answerable: can_answer
            && value.state == QuestionStatus::Pending
            && !value.questions.is_empty()
            && (!value.questions.iter().any(|question| question.secret)
                || (value.mode == QuestionMode::Native && value.blocking && local_operator)),
        answered_elsewhere: value.answered_elsewhere,
        status: match value.state {
            QuestionStatus::Pending => dto::Status::Pending,
            QuestionStatus::Submitting => dto::Status::Submitting,
            QuestionStatus::Answered => dto::Status::Answered,
            QuestionStatus::Closed => dto::Status::Closed,
            QuestionStatus::Unavailable => dto::Status::Unavailable,
            QuestionStatus::Uncertain => dto::Status::Uncertain,
        },
        questions: value.questions.iter().map(project_question).collect(),
        locator,
    })
}

pub(super) fn project_question(value: &Question) -> dto::Question {
    dto::Question {
        id: value.id.clone(),
        header: value.header.clone(),
        question: value.prompt.clone(),
        options: value
            .options
            .iter()
            .map(|option| dto::Choice {
                label: option.label.clone(),
                description: option.description.clone(),
                preview: option.preview.clone(),
            })
            .collect(),
        is_other: value.allows_other,
        is_secret: value.secret,
        multi_select: value.multi_select,
    }
}

fn run_id(run: &RunId) -> Result<RunIdDto, AppError> {
    RunIdDto::new(run.as_str()).ok_or(AppError::Internal)
}

fn session_key(session: &SessionKey) -> Result<SessionKeyDto, AppError> {
    SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)
}

fn question_port<'a>(entry: &'a Entry, identity: &NativeIdentity) -> Option<&'a dyn QuestionsPort> {
    let port: &dyn QuestionsPort = match identity.provider() {
        HostProvider::Codex => entry.host.as_deref()?,
        HostProvider::Claude => entry.claude.as_deref()?,
    };
    port.supports_questions(identity).then_some(port)
}

fn validate_request(request: Option<&str>) -> Result<(), AppError> {
    if request.is_some_and(|value| ResourceIdDto::new(value).is_none()) {
        return Err(AppError::InvalidRequest);
    }
    Ok(())
}
