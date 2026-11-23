use std::sync::Mutex;
use std::time::{Duration, Instant};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{
    ClaudeQuestionAddress, ClaudeQuestionAnswers, QuestionAccess, QuestionAnswer, QuestionForm,
    QuestionMode, QuestionStatus, QuestionsPort,
};
use reviewq_domain::identity::SubmissionId;
use reviewq_domain::session::{HostProvider, NativeIdentity};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use super::Claude;
use crate::history::claude_questions;
use crate::question_wire::request_id;

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
    submission: Option<(SubmissionId, [u8; 32])>,
}

impl Claude {
    pub(super) async fn watch_question(
        &self,
        session: HistorySession,
        address: ClaudeQuestionAddress,
    ) -> Result<oneshot::Receiver<ClaudeQuestionAnswers>, AppError> {
        let questions = self
            .read_native(session.clone(), {
                let address = address.clone();
                move |source, cancelled| claude_questions::open(&source, &address, cancelled)
            })
            .await?;
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
                return if submission == &(answer.submission, digest) {
                    Ok(entry.form.clone())
                } else {
                    Err(AppError::Conflict)
                };
            }
            if entry.form.state != QuestionStatus::Pending {
                return Err(AppError::StaleRevision);
            }
            let callback = entry.callback.take().ok_or(AppError::Unavailable)?;
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
            if callback.send(answers).is_err() {
                entry.form.state = QuestionStatus::Unavailable;
                entry.expires = Some(Instant::now() + RETAIN_FOR);
                return Err(AppError::Unavailable);
            }
            entry.submission = Some((answer.submission, digest));
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
