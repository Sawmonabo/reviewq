use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{
    ClaudeQuestionAddress, ClaudeQuestionAnswers, QuestionAccess, QuestionAnswer, QuestionForm,
    QuestionMode, QuestionStatus, QuestionsPort, claude_answer_digest,
};
use reviewq_domain::identity::{ReportHash, SubmissionId};
use reviewq_domain::session::{HostProvider, NativeIdentity};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use super::Claude;
use crate::question_wire::{claude, request_id};

const MAX_FORMS: usize = 64;
const MAX_SESSION_FORMS: usize = 8;
const RETAIN_FOR: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct Questions(Mutex<Vec<LiveQuestion>>);

struct LiveQuestion {
    session: HistorySession,
    address: ClaudeQuestionAddress,
    form: QuestionForm,
    expires: Option<Instant>,
    callback: Option<oneshot::Sender<ClaudeQuestionAnswers>>,
    submission: Option<Submission>,
}

struct Submission {
    id: SubmissionId,
    browser: [u8; 32],
    native: ReportHash,
}

impl Claude {
    pub(super) fn watch_question(
        &self,
        session: HistorySession,
        address: ClaudeQuestionAddress,
        input: &[u8],
    ) -> Result<oneshot::Receiver<ClaudeQuestionAnswers>, AppError> {
        if session.parent.is_some() || !self.supports_questions(session.binding.identity()) {
            return Err(AppError::Forbidden);
        }
        let questions = claude::project(input).ok_or(AppError::Unavailable)?;
        let mut prompts = HashSet::new();
        if questions
            .iter()
            .any(|question| question.secret || !prompts.insert(&question.prompt))
        {
            return Err(AppError::Unavailable);
        }
        let request = request_id(
            session.binding.identity().native_id().as_str(),
            address.assistant.as_str(),
            address.call.as_str(),
        );
        let mut live = self.questions.0.lock().map_err(|_| AppError::Internal)?;
        retain_current(&mut live);
        if live
            .iter()
            .any(|entry| entry.matches(&session, Some(&request)))
        {
            return Err(AppError::Conflict);
        }
        if live.len() >= MAX_FORMS
            || live
                .iter()
                .filter(|entry| entry.matches(&session, None))
                .count()
                >= MAX_SESSION_FORMS
        {
            return Err(AppError::Busy);
        }
        let (callback, receiver) = oneshot::channel();
        live.push(LiveQuestion {
            session,
            form: QuestionForm {
                request,
                turn: address.turn.clone(),
                item: address.assistant.clone(),
                blocking: true,
                mode: QuestionMode::Native,
                state: QuestionStatus::Pending,
                answered_elsewhere: false,
                questions,
                locator: None,
            },
            address,
            expires: None,
            callback: Some(callback),
            submission: None,
        });
        Ok(receiver)
    }

    pub(super) fn settle_question(
        &self,
        session: &HistorySession,
        address: &ClaudeQuestionAddress,
        status: QuestionStatus,
        answer_digest: Option<ReportHash>,
    ) -> bool {
        let Ok(mut live) = self.questions.0.lock() else {
            return false;
        };
        let Some(entry) = live
            .iter_mut()
            .find(|entry| entry.matches(session, None) && entry.address == *address)
        else {
            return false;
        };
        if matches!(
            entry.form.state,
            QuestionStatus::Answered | QuestionStatus::Closed
        ) {
            return false;
        }
        entry.callback = None;
        if entry.form.state == status {
            return false;
        }
        entry.form.state = status;
        entry.form.answered_elsewhere = status == QuestionStatus::Answered
            && !entry
                .submission
                .as_ref()
                .is_some_and(|submission| answer_digest.as_ref() == Some(&submission.native));
        entry.expires = Some(Instant::now() + RETAIN_FOR);
        true
    }
}

impl QuestionsPort for Claude {
    fn supports_questions(&self, identity: &NativeIdentity) -> bool {
        identity.provider() == HostProvider::Claude && identity.profile() == &self.profile
    }

    fn questions(
        &self,
        access: QuestionAccess,
        request: Option<String>,
    ) -> AppFuture<'_, Vec<QuestionForm>> {
        Box::pin(async move {
            let mut live = self.questions.0.lock().map_err(|_| AppError::Internal)?;
            retain_current(&mut live);
            Ok(live
                .iter()
                .filter(|entry| entry.matches(access.session(), request.as_deref()))
                .map(|entry| entry.form.clone())
                .collect())
        })
    }

    fn answer_question(&self, answer: QuestionAnswer) -> AppFuture<'_, QuestionForm> {
        Box::pin(async move {
            answer.check().await?;
            let mut live = self.questions.0.lock().map_err(|_| AppError::Internal)?;
            retain_current(&mut live);
            let entry = live
                .iter_mut()
                .find(|entry| entry.matches(answer.session(), Some(&answer.request)))
                .ok_or(AppError::NotFound)?;
            answer.require_address()?;
            answer.validate(&entry.form)?;
            let digest = Sha256::digest(
                serde_json::to_vec(&answer.answers).map_err(|_| AppError::InvalidRequest)?,
            )
            .into();
            if let Some(submission) = &entry.submission {
                return if submission.id == answer.submission
                    && submission.browser == digest
                    && !entry.form.answered_elsewhere
                {
                    Ok(entry.form.clone())
                } else {
                    Err(AppError::Conflict)
                };
            }
            if entry.form.state != QuestionStatus::Pending {
                return Err(AppError::StaleRevision);
            }
            let answers = entry
                .form
                .questions
                .iter()
                .map(|question| {
                    (
                        question.prompt.clone(),
                        answer.answers[&question.id].join(", "),
                    )
                })
                .collect();
            let native = claude_answer_digest(&answers).ok_or(AppError::InvalidRequest)?;
            let callback = entry.callback.take().ok_or(AppError::Unavailable)?;
            if callback.send(answers).is_err() {
                entry.form.state = QuestionStatus::Unavailable;
                entry.expires = Some(Instant::now() + RETAIN_FOR);
                return Err(AppError::Unavailable);
            }
            entry.submission = Some(Submission {
                id: answer.submission,
                browser: digest,
                native,
            });
            entry.form.state = QuestionStatus::Submitting;
            entry.expires = Some(Instant::now() + RETAIN_FOR);
            Ok(entry.form.clone())
        })
    }
}

impl LiveQuestion {
    fn matches(&self, other: &HistorySession, request: Option<&str>) -> bool {
        other.parent.is_none()
            && self.session.target == other.target
            && self.session.binding == other.binding
            && request.is_none_or(|request| self.form.request == request)
    }
}

fn retain_current(live: &mut Vec<LiveQuestion>) {
    let now = Instant::now();
    live.retain_mut(|entry| {
        if entry.form.state == QuestionStatus::Pending
            && entry
                .callback
                .as_ref()
                .is_some_and(oneshot::Sender::is_closed)
        {
            entry.form.state = QuestionStatus::Unavailable;
            entry.callback = None;
            entry.expires = Some(now + RETAIN_FOR);
        }
        entry.expires.is_none_or(|expires| expires > now)
    });
}
