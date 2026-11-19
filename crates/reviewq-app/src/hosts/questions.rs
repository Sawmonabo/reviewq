use std::collections::BTreeMap;
use std::sync::Arc;

use reviewq_domain::identity::{NativeInputId, NativeTurnId, ReceiptId, ReportHash, SubmissionId};
use reviewq_domain::session::NativeIdentity;

use crate::access::{Actor, ActorScope, AppError, AppFuture};
use crate::history::{HistorySession, HistoryStore};
use sha2::{Digest, Sha256};

#[derive(Clone, Eq, PartialEq)]
pub struct Question {
    pub id: String,
    pub header: String,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
    pub allows_other: bool,
    pub secret: bool,
}

#[derive(Clone, Eq, PartialEq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Eq, PartialEq)]
pub struct QuestionForm {
    pub request: String,
    pub turn: NativeTurnId,
    pub item: NativeInputId,
    pub blocking: bool,
    pub mode: QuestionMode,
    pub state: QuestionStatus,
    pub questions: Vec<Question>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum QuestionMode {
    Native,
    Async,
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

impl QuestionStatus {
    pub fn can_settle(self) -> bool {
        matches!(self, Self::Submitting | Self::Uncertain)
    }
}

#[derive(Clone)]
pub struct QuestionAccess {
    actor: Actor,
    session: HistorySession,
    store: Arc<dyn QuestionStore>,
}

impl QuestionAccess {
    pub fn new(actor: Actor, session: HistorySession, store: Arc<dyn QuestionStore>) -> Self {
        Self {
            actor,
            session,
            store,
        }
    }

    pub fn session(&self) -> &HistorySession {
        &self.session
    }

    pub async fn attempts(
        &self,
        request: Option<String>,
    ) -> Result<Vec<QuestionReceipt>, AppError> {
        self.store
            .question_attempts(self.actor.clone(), self.session.clone(), request)
            .await
    }

    pub async fn settle(
        &self,
        receipt: &QuestionReceipt,
        state: QuestionStatus,
    ) -> Result<(), AppError> {
        self.store
            .settle_question(
                self.actor.clone(),
                self.session.clone(),
                receipt.key.clone(),
                receipt.attempt_id.clone(),
                state,
            )
            .await
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct QuestionAnchor {
    pub generation: String,
    pub opening: u64,
}

#[derive(Clone, Eq, PartialEq)]
pub struct QuestionKey {
    pub request: String,
    pub turn: NativeTurnId,
    pub item: NativeInputId,
    pub anchor: QuestionAnchor,
}

pub struct ObservedAsyncQuestion {
    pub key: QuestionKey,
    pub line_sha256: ReportHash,
    pub state: QuestionStatus,
}

pub struct QuestionReceipt {
    pub attempt_id: ReceiptId,
    pub key: QuestionKey,
    pub blocking: bool,
    pub state: QuestionStatus,
    /// Secret responses have no durable payload or content digest.
    pub digest: Option<[u8; 32]>,
}

impl QuestionReceipt {
    pub fn matches(&self, form: &QuestionForm) -> bool {
        self.key.request == form.request && self.key.turn == form.turn && self.key.item == form.item
    }

    pub fn form(&self) -> QuestionForm {
        QuestionForm {
            request: self.key.request.clone(),
            turn: self.key.turn.clone(),
            item: self.key.item.clone(),
            blocking: self.blocking,
            mode: QuestionMode::Native,
            state: self.state,
            questions: Vec::new(),
        }
    }
}

pub enum QuestionAdmission {
    Reserved(QuestionReceipt),
    Existing(QuestionReceipt),
}

pub trait QuestionStore: HistoryStore + Send + Sync {
    fn observed_async_questions(
        &self,
        actor: Actor,
        session: HistorySession,
        request: Option<String>,
    ) -> AppFuture<'_, Vec<ObservedAsyncQuestion>>;

    fn question_attempts(
        &self,
        actor: Actor,
        session: HistorySession,
        request: Option<String>,
    ) -> AppFuture<'_, Vec<QuestionReceipt>>;

    fn replay_question(
        &self,
        actor: Actor,
        session: HistorySession,
        request: String,
        submission: SubmissionId,
        digest: [u8; 32],
    ) -> AppFuture<'_, Option<QuestionReceipt>>;

    fn reserve_question(
        &self,
        actor: Actor,
        session: HistorySession,
        submission: SubmissionId,
        receipt: QuestionReceipt,
    ) -> AppFuture<'_, QuestionAdmission>;

    fn settle_question(
        &self,
        actor: Actor,
        session: HistorySession,
        key: QuestionKey,
        attempt_id: ReceiptId,
        state: QuestionStatus,
    ) -> AppFuture<'_, ()>;
}

pub struct QuestionAnswer {
    pub request: String,
    pub submission: SubmissionId,
    pub answers: BTreeMap<String, Vec<String>>,
    pub access: QuestionAccess,
}

impl QuestionAnswer {
    pub fn new(
        request: String,
        submission: SubmissionId,
        answers: BTreeMap<String, Vec<String>>,
        access: QuestionAccess,
    ) -> Self {
        Self {
            request,
            submission,
            answers,
            access,
        }
    }

    pub fn identity(&self) -> &NativeIdentity {
        self.access.session.binding.identity()
    }

    pub fn session(&self) -> &HistorySession {
        &self.access.session
    }

    pub fn allows_secret(&self) -> bool {
        matches!(self.access.actor.scope(), ActorScope::LocalOperator)
    }

    pub fn digest(&self) -> Result<[u8; 32], AppError> {
        answer_digest(&self.answers)
    }

    pub fn validate(&self, form: &QuestionForm) -> Result<bool, AppError> {
        let secret = form.questions.iter().any(|question| question.secret);
        if (secret && (!form.blocking || !self.allows_secret()))
            || self.answers.len() != form.questions.len()
        {
            return Err(AppError::InvalidRequest);
        }
        for question in &form.questions {
            let Some([value]) = self.answers.get(&question.id).map(Vec::as_slice) else {
                return Err(AppError::InvalidRequest);
            };
            if value.is_empty()
                || value.len() > 8192
                || value
                    .chars()
                    .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
                || (!question.allows_other
                    && !question.options.is_empty()
                    && !question.options.iter().any(|option| &option.label == value))
            {
                return Err(AppError::InvalidRequest);
            }
        }
        Ok(secret)
    }

    pub async fn replay(&self, digest: [u8; 32]) -> Result<Option<QuestionReceipt>, AppError> {
        self.access
            .store
            .replay_question(
                self.access.actor.clone(),
                self.access.session.clone(),
                self.request.clone(),
                self.submission.clone(),
                digest,
            )
            .await
    }

    pub async fn reserve(&self, receipt: QuestionReceipt) -> Result<QuestionAdmission, AppError> {
        self.access
            .store
            .reserve_question(
                self.access.actor.clone(),
                self.access.session.clone(),
                self.submission.clone(),
                receipt,
            )
            .await
    }

    pub async fn check(&self) -> Result<(), AppError> {
        let current = self
            .access
            .store
            .history_session(
                self.access.actor.clone(),
                self.access.session.target.run_id().clone(),
                self.access.session.target.session_key().clone(),
            )
            .await?;
        if current.value != self.access.session {
            return Err(AppError::Conflict);
        }
        Ok(())
    }
}

/// Canonical comparison shared by admission and exact native result observation.
pub fn answer_digest(answers: &BTreeMap<String, Vec<String>>) -> Result<[u8; 32], AppError> {
    if answers.len() > 16 {
        return Err(AppError::InvalidRequest);
    }
    let mut total = 0;
    let mut digest = Sha256::new();
    for (id, values) in answers {
        let [value] = values.as_slice() else {
            return Err(AppError::InvalidRequest);
        };
        if id.len() > 256 || value.len() > 8_192 {
            return Err(AppError::InvalidRequest);
        }
        total += id.len() + value.len();
        for field in [id, value] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
    }
    if total > 48 * 1024 {
        return Err(AppError::PayloadTooLarge);
    }
    Ok(digest.finalize().into())
}

pub trait QuestionsPort: Send + Sync {
    fn questions(
        &self,
        access: QuestionAccess,
        request: Option<String>,
    ) -> AppFuture<'_, Vec<QuestionForm>>;
    fn answer_question(&self, answer: QuestionAnswer) -> AppFuture<'_, QuestionForm>;
}
