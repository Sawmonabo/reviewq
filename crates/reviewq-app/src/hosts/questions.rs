use std::collections::BTreeMap;
use std::sync::Arc;

use reviewq_domain::identity::{NativeInputId, NativeTurnId, SubmissionId};
use reviewq_domain::session::NativeIdentity;

use crate::access::{Actor, ActorScope, AppError, AppFuture};
use crate::history::{HistorySession, HistoryStore};

#[derive(Clone)]
pub struct Question {
    pub id: String,
    pub header: String,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
    pub allows_other: bool,
    pub secret: bool,
}

#[derive(Clone)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone)]
pub struct QuestionForm {
    pub request: String,
    pub turn: NativeTurnId,
    pub item: NativeInputId,
    pub blocking: bool,
    pub state: QuestionStatus,
    pub questions: Vec<Question>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum QuestionStatus {
    Pending,
    Submitting,
    Answered,
    Closed,
    Unavailable,
    Uncertain,
}

pub struct QuestionAnswer {
    pub request: String,
    pub submission: SubmissionId,
    pub answers: BTreeMap<String, Vec<String>>,
    session: HistorySession,
    actor: Actor,
    store: Arc<dyn HistoryStore>,
}

impl QuestionAnswer {
    pub fn new(
        request: String,
        submission: SubmissionId,
        answers: BTreeMap<String, Vec<String>>,
        session: HistorySession,
        actor: Actor,
        store: Arc<dyn HistoryStore>,
    ) -> Self {
        Self {
            request,
            submission,
            answers,
            session,
            actor,
            store,
        }
    }

    pub fn identity(&self) -> &NativeIdentity {
        self.session.binding.identity()
    }

    pub fn session(&self) -> &HistorySession {
        &self.session
    }

    pub fn allows_secret(&self) -> bool {
        matches!(self.actor.scope(), ActorScope::LocalOperator)
    }

    pub async fn check(&self) -> Result<(), AppError> {
        let current = self
            .store
            .history_session(
                self.actor.clone(),
                self.session.target.run_id().clone(),
                self.session.target.session_key().clone(),
            )
            .await?;
        if current.value.binding != self.session.binding {
            return Err(AppError::Conflict);
        }
        Ok(())
    }
}

pub trait QuestionsPort: Send + Sync {
    fn questions(
        &self,
        session: HistorySession,
        request: Option<String>,
    ) -> AppFuture<'_, Vec<QuestionForm>>;
    fn answer_question(&self, answer: QuestionAnswer) -> AppFuture<'_, QuestionForm>;
}
