use reviewq_app::access::{Actor, AppError};
use reviewq_app::followups::{MonitorCommand, MonitorRequest, MonitorState};
use reviewq_domain::identity::{ReviewId, Revision, RunId, SessionKey, SubmissionId};
use reviewq_protocol::mcp::{
    AnswerReviewQuestion, ConfigureFollowups, FollowupAction, StartReview,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::entry::features::followups::{execute, project_receipt, selection};
use crate::entry::pages::parse;
use crate::entry::questions;
use crate::entry::{Entry, launch};

use super::operations::{ResolvedReview, alternatives, dashboard, resolve, run_summary};

pub(super) async fn start(
    entry: &Entry,
    actor: Actor,
    args: StartReview,
) -> Result<Value, AppError> {
    let canonical = serde_json::to_vec(&args).map_err(|_| AppError::Internal)?;
    let digest: [u8; 32] = Sha256::digest(canonical).into();
    let submission: SubmissionId = parse(args.submission_id.as_str())?;
    if let Some(found) = entry
        .store
        .launch_receipt(actor.clone(), None, submission.clone(), Some(digest))
        .await?
    {
        let stored = launch::reconcile(entry, actor, found).await?;
        return launch_output(entry, stored.value);
    }
    let review = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id) => id,
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let stored = launch::execute(
        entry,
        actor,
        launch::LaunchCommand {
            review: review.clone(),
            provider: launch::provider(args.provider),
            submission,
            selection: launch::selection(args.invocation, args.context, args.model, args.effort)?,
            expected_revision: None,
            expected_sha: args
                .expected_sha
                .map(|sha| parse(sha.as_str()))
                .transpose()?,
            digest,
        },
    )
    .await?;
    launch_output(entry, stored.value)
}

fn launch_output(
    entry: &Entry,
    stored: reviewq_app::launch::LaunchReceipt,
) -> Result<Value, AppError> {
    let url = dashboard(entry, &stored.review, stored.run.as_ref());
    let receipt = launch::project_receipt(stored)?;
    let session = if matches!(receipt.state, reviewq_protocol::launch::State::Confirmed) {
        Some(
            json!({"runId":receipt.run_id, "sessionKey":receipt.session_key,"nativeId":receipt.native_id}),
        )
    } else {
        None
    };
    Ok(json!({"session":session,"dashboard":url,"receipt":receipt}))
}

pub(super) async fn followups(
    entry: &Entry,
    actor: Actor,
    args: ConfigureFollowups,
) -> Result<Value, AppError> {
    let review = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id) => id,
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let run = args
        .run_id
        .map(|value| parse::<RunId>(value.as_str()))
        .transpose()?;
    let session = args
        .session_key
        .map(|value| parse::<SessionKey>(value.as_str()))
        .transpose()?;
    let explicit = exact_lead(entry, &actor, &review, run, session.as_ref()).await?;
    let command = match args.action {
        FollowupAction::Configure => {
            let run = match explicit {
                Some(run) => run,
                None => {
                    let choices = entry
                        .store
                        .management_runs(actor.clone(), review.clone(), 0, 10)
                        .await?
                        .value;
                    if choices.total > 1 {
                        let choices: Vec<_> = choices
                            .runs
                            .iter()
                            .map(|run| run_summary(entry, &review, run))
                            .collect();
                        return Ok(
                            json!({"state":"ambiguous","choices":choices,"dashboard":dashboard(entry,&review,None)}),
                        );
                    }
                    choices
                        .runs
                        .into_iter()
                        .next()
                        .ok_or(AppError::NotFound)?
                        .run_id
                }
            };
            let state = match args
                .state
                .unwrap_or(reviewq_protocol::followups::EnrollmentState::Enabled)
            {
                reviewq_protocol::followups::EnrollmentState::Enabled => MonitorState::Enabled,
                reviewq_protocol::followups::EnrollmentState::Paused => MonitorState::Paused,
            };
            let selections = args
                .threads
                .ok_or(AppError::InvalidRequest)?
                .into_iter()
                .map(selection)
                .collect();
            MonitorCommand::Configure {
                run,
                state,
                selections,
            }
        }
        other => {
            if args.threads.is_some() || args.state.is_some() {
                return Err(AppError::InvalidRequest);
            }
            if let Some(run) = explicit {
                let enrollment = entry
                    .store
                    .read_followups(actor.clone(), review.clone())
                    .await?
                    .value;
                if enrollment.monitor.and_then(|item| item.run).as_ref() != Some(&run) {
                    return Err(AppError::Conflict);
                }
            }
            match other {
                FollowupAction::Pause => MonitorCommand::Pause,
                FollowupAction::Resume => MonitorCommand::Resume,
                FollowupAction::ReviewNow if entry.sends_allowed => MonitorCommand::ReviewNow,
                _ => return Err(AppError::Forbidden),
            }
        }
    };
    let result = execute(
        entry,
        actor,
        MonitorRequest {
            review: review.clone(),
            submission: parse(args.submission_id.as_str())?,
            expected_generation: args
                .expected_generation
                .map(|value| Revision::try_from(value.get()).map_err(|_| AppError::InvalidRequest))
                .transpose()?,
            command,
        },
    )
    .await?;
    Ok(json!({
        "submissionId":result.value.submission.as_str(), "followups":project_receipt(result.value)?,
        "dashboard":dashboard(entry, &review, None),
    }))
}

pub(super) async fn answer(
    entry: &Entry,
    actor: Actor,
    args: AnswerReviewQuestion,
) -> Result<Value, AppError> {
    let review = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id) => id,
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let (run, session) = if let Some(session) = args.session_key {
        let session: SessionKey = parse(session.as_str())?;
        let run = entry
            .store
            .run_for_session(actor.clone(), review.clone(), session.clone())
            .await?;
        (run, session)
    } else {
        let choices = entry
            .store
            .management_runs(actor.clone(), review.clone(), 0, 10)
            .await?
            .value;
        if choices.total > 1 {
            let choices: Vec<_> = choices
                .runs
                .iter()
                .map(|run| run_summary(entry, &review, run))
                .collect();
            return Ok(
                json!({"state":"ambiguous","choices":choices,"dashboard":dashboard(entry,&review,None)}),
            );
        }
        let selected = choices.runs.into_iter().next().ok_or(AppError::NotFound)?;
        (selected.run_id, selected.lead_session_key)
    };
    let result = questions::respond(
        entry,
        actor,
        run,
        session,
        args.request_id.as_str().to_owned(),
        reviewq_protocol::questions::Answer {
            submission_id: args.submission_id,
            answers: args.answers,
        },
    )
    .await?;
    serde_json::to_value(result.data).map_err(|_| AppError::Internal)
}

async fn exact_lead(
    entry: &Entry,
    actor: &Actor,
    review: &ReviewId,
    mut run: Option<RunId>,
    session: Option<&SessionKey>,
) -> Result<Option<RunId>, AppError> {
    if let Some(session) = session {
        let found = entry
            .store
            .run_for_session(actor.clone(), review.clone(), session.clone())
            .await?;
        if run.as_ref().is_some_and(|run| run != &found) {
            return Err(AppError::NotFound);
        }
        run = Some(found);
    }
    if let Some(run) = &run {
        let found = entry
            .store
            .resolve_selection(actor.clone(), review.clone(), Some(run.clone()))
            .await?
            .value;
        let lead = found.lead().ok_or(AppError::NotFound)?;
        if session.is_some_and(|session| session != lead.session_key()) {
            return Err(AppError::NotFound);
        }
    }
    Ok(run)
}
