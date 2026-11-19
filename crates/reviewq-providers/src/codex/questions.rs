use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{
    Question, QuestionAccess, QuestionAdmission, QuestionAnswer, QuestionForm, QuestionKey,
    QuestionOption, QuestionReceipt, QuestionStatus, QuestionsPort,
};
use reviewq_domain::identity::ReportHash;
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::Codex;
use super::connection::AnswerError;
use super::session::{Thread, ThreadStatus, canonical_cwd};

const MAX_FORMS: usize = 64;
const MAX_QUESTIONS: usize = 16;
const MAX_OPTIONS: usize = 32;
const MAX_RESULT_SECONDS: u64 = 8;
const FRAME_BUDGET: usize = 48 * 1024;
const CACHE_LIFETIME: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
pub(super) struct Questions(HashMap<String, Live>);

struct Live {
    thread: String,
    native_id: Value,
    form: QuestionForm,
    seen: Instant,
    submission: Option<Submission>,
}

struct Submission {
    expires: Instant,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeForm {
    thread_id: String,
    turn_id: String,
    item_id: String,
    is_blocking: bool,
    questions: Vec<NativeQuestion>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeQuestion {
    id: String,
    header: String,
    question: String,
    options: Option<Vec<NativeOption>>,
    #[serde(default)]
    is_other: bool,
    #[serde(default)]
    is_secret: bool,
}

#[derive(Deserialize)]
struct NativeOption {
    label: String,
    description: String,
}

impl QuestionsPort for Codex {
    fn questions(
        &self,
        access: QuestionAccess,
        request: Option<String>,
    ) -> AppFuture<'_, Vec<QuestionForm>> {
        Box::pin(async move {
            let session = access.session();
            let native = session.binding.identity();
            if native.profile() != &self.profile || native.provider() != HostProvider::Codex {
                return Err(AppError::NotFound);
            }
            let mut forms = self
                .connection
                .questions(native.native_id().as_str(), request.as_deref())?;
            // A remembered socket handle can only describe the currently bound cwd and lineage.
            // Durable receipts below remain readable with their independently verified file.
            if !forms.is_empty() && self.question_thread(session, false).await.is_err() {
                forms.clear();
            }
            let focused = request.is_some();
            let receipts = access.attempts(request).await?;
            let mut active = receipts
                .iter()
                .filter(|receipt| needs_question_proof(receipt, focused))
                .take(2)
                .count();
            let verify = if active != 0 {
                self.question_proofs.try_acquire().ok()
            } else {
                None
            };
            let path = if verify.is_some() {
                self.question_readback_path(session).await
            } else {
                None
            };
            for mut receipt in receipts {
                let form = forms.iter_mut().find(|form| receipt.matches(form));
                if needs_question_proof(&receipt, focused)
                    && active != 0
                    && let Some(path) = &path
                {
                    active -= 1;
                    self.reconcile_question(&access, &mut receipt, path.clone())
                        .await?;
                }
                match form {
                    Some(form) => {
                        if !(receipt.state == QuestionStatus::Unavailable
                            && form.state == QuestionStatus::Pending)
                        {
                            form.state = display_status(receipt.state, Some(form.state));
                        }
                    }
                    None => {
                        let mut form = receipt.form();
                        form.state = display_status(receipt.state, None);
                        forms.push(form);
                    }
                }
            }
            forms.sort_by_key(|form| {
                !matches!(
                    form.state,
                    QuestionStatus::Pending | QuestionStatus::Submitting
                )
            });
            forms.truncate(8);
            Ok(forms)
        })
    }

    fn answer_question(&self, answer: QuestionAnswer) -> AppFuture<'_, QuestionForm> {
        Box::pin(async move {
            if answer.identity().profile() != &self.profile
                || answer.identity().provider() != HostProvider::Codex
            {
                return Err(AppError::NotFound);
            }
            let digest = answer.digest()?;
            if let Some(mut saved) = answer.replay(digest).await? {
                if needs_question_proof(&saved, true)
                    && let Ok(_permit) = self.question_proofs.try_acquire()
                    && let Some(path) = self.question_readback_path(answer.session()).await
                {
                    self.reconcile_question(&answer.access, &mut saved, path)
                        .await?;
                }
                return self.receipt_form(&answer.access, saved);
            }
            let forms = self.connection.questions(
                answer.identity().native_id().as_str(),
                Some(&answer.request),
            )?;
            let Some(form) = forms.into_iter().next() else {
                return Err(AppError::NotFound);
            };
            if form.state != QuestionStatus::Pending {
                return Ok(form);
            }
            let secret = answer.validate(&form)?;
            let _permit = self
                .question_proofs
                .try_acquire()
                .map_err(|_| AppError::Busy)?;
            let session = answer.session();
            let thread = self.question_thread(session, true).await?;
            if form.blocking {
                if !matches!(thread.status, ThreadStatus::Active { .. }) {
                    return Err(AppError::Conflict);
                }
                let page = self.turns(&thread.id, None, false, 1, false).await?;
                if !page.data.first().is_some_and(|turn| {
                    turn.id == form.turn.as_str() && turn.status == "inProgress"
                }) {
                    return Err(AppError::Conflict);
                }
            }
            let path = thread.path.ok_or(AppError::Unavailable)?;
            let anchor = crate::history::codex_question_opening(
                session.clone(),
                path,
                form.item.as_str().to_owned(),
                form.turn.clone(),
            )
            .await?;
            let attempted = QuestionReceipt {
                attempt_id: uuid::Uuid::new_v4()
                    .to_string()
                    .parse()
                    .map_err(|_| AppError::Internal)?,
                key: QuestionKey {
                    request: form.request.clone(),
                    turn: form.turn.clone(),
                    item: form.item.clone(),
                    anchor,
                },
                blocking: form.blocking,
                state: QuestionStatus::Submitting,
                digest: (!secret).then_some(digest),
            };
            let receipt = match answer.reserve(attempted).await? {
                QuestionAdmission::Existing(receipt) => {
                    return self.receipt_form(&answer.access, receipt);
                }
                QuestionAdmission::Reserved(receipt) => receipt,
            };
            let access = answer.access.clone();
            match self.connection.answer(answer, form.clone()).await {
                Ok(current) if current.state == QuestionStatus::Submitting => Ok(current),
                Ok(current) => {
                    if current.state == QuestionStatus::Unavailable {
                        access.settle(&receipt, QuestionStatus::Unavailable).await?;
                    }
                    Ok(current)
                }
                Err(failure) => {
                    let state = match failure {
                        AnswerError::BeforeSend => QuestionStatus::Unavailable,
                        AnswerError::Uncertain => QuestionStatus::Uncertain,
                    };
                    access.settle(&receipt, state).await?;
                    Ok(QuestionForm { state, ..form })
                }
            }
        })
    }
}

impl Codex {
    async fn question_thread(
        &self,
        session: &HistorySession,
        active: bool,
    ) -> Result<Thread, AppError> {
        let thread = self
            .read_thread(session.binding.identity().native_id())
            .await?;
        let cwd_matches = if active {
            canonical_cwd(thread.cwd.clone()).await? == *session.binding.cwd()
        } else {
            super::session::retained_cwd(&thread, session.binding.cwd()).await?
        };
        if !cwd_matches
            || thread.parent_thread_id.as_deref()
                != session
                    .parent
                    .as_ref()
                    .map(|parent| parent.native_id.as_str())
        {
            return Err(AppError::Conflict);
        }
        Ok(thread)
    }

    async fn question_readback_path(&self, session: &HistorySession) -> Option<PathBuf> {
        if let Some(path) = &session.source {
            return Some(path.as_path().to_owned());
        }
        self.question_thread(session, false)
            .await
            .ok()
            .and_then(|thread| thread.path)
    }

    fn receipt_form(
        &self,
        access: &QuestionAccess,
        receipt: QuestionReceipt,
    ) -> Result<QuestionForm, AppError> {
        let forms = self.connection.questions(
            access.session().binding.identity().native_id().as_str(),
            Some(&receipt.key.request),
        )?;
        let live = forms.into_iter().find(|form| receipt.matches(form));
        let state = display_status(receipt.state, live.as_ref().map(|form| form.state));
        Ok(QuestionForm {
            state,
            ..live.unwrap_or_else(|| receipt.form())
        })
    }

    async fn reconcile_question(
        &self,
        access: &QuestionAccess,
        receipt: &mut QuestionReceipt,
        path: PathBuf,
    ) -> Result<(), AppError> {
        if let Ok(Some(state)) = crate::history::codex_question_answered(
            access.session().clone(),
            path,
            receipt.key.clone(),
            receipt.digest,
        )
        .await
        {
            access.settle(receipt, state).await?;
            receipt.state = state;
        }
        Ok(())
    }
}

fn needs_question_proof(receipt: &QuestionReceipt, focused: bool) -> bool {
    receipt.state.can_settle()
        || (focused && receipt.state == QuestionStatus::Closed && receipt.digest.is_some())
}

fn display_status(durable: QuestionStatus, live: Option<QuestionStatus>) -> QuestionStatus {
    match (durable, live) {
        (QuestionStatus::Submitting, Some(QuestionStatus::Submitting)) => {
            QuestionStatus::Submitting
        }
        (QuestionStatus::Submitting | QuestionStatus::Uncertain, Some(QuestionStatus::Closed)) => {
            QuestionStatus::Closed
        }
        (QuestionStatus::Submitting, _) => QuestionStatus::Uncertain,
        (durable, _) => durable,
    }
}

impl Questions {
    pub(super) fn frames(&mut self, frame: &Value) {
        match frame.get("method").and_then(Value::as_str) {
            Some("item/tool/requestUserInput") => {
                if let Some(id) = frame.get("id").filter(|id| valid_native_id(id))
                    && let Some(params) = frame.get("params")
                    && let Ok(encoded) = serde_json::to_vec(params)
                    && encoded.len() <= FRAME_BUDGET
                    && let Ok(input) = serde_json::from_slice::<NativeForm>(&encoded)
                {
                    self.insert(id.clone(), input);
                }
            }
            Some("serverRequest/resolved") => {
                let Some(thread) = frame.pointer("/params/threadId").and_then(Value::as_str) else {
                    return;
                };
                let Some(id) = frame.pointer("/params/requestId") else {
                    return;
                };
                for live in self
                    .0
                    .values_mut()
                    .filter(|live| live.thread == thread && &live.native_id == id)
                {
                    live.form.state = match live.form.state {
                        QuestionStatus::Pending
                        | QuestionStatus::Submitting
                        | QuestionStatus::Uncertain => QuestionStatus::Closed,
                        existing => existing,
                    };
                }
            }
            Some("turn/completed") => {
                let Some(thread) = frame.pointer("/params/threadId").and_then(Value::as_str) else {
                    return;
                };
                let Some(turn) = frame.pointer("/params/turn/id").and_then(Value::as_str) else {
                    return;
                };
                for live in self
                    .0
                    .values_mut()
                    .filter(|live| live.thread == thread && live.form.turn.as_str() == turn)
                {
                    if live.form.state == QuestionStatus::Pending {
                        live.form.state = QuestionStatus::Closed;
                    }
                }
            }
            _ => {}
        }
    }

    fn insert(&mut self, native_id: Value, native: NativeForm) {
        self.prune();
        if self.0.len() >= MAX_FORMS
            || !visible(&native.thread_id, 256, false)
            || native.questions.is_empty()
            || native.questions.len() > MAX_QUESTIONS
            || self.0.values().any(|live| {
                live.thread == native.thread_id
                    && live.native_id == native_id
                    && (live.form.state != QuestionStatus::Unavailable || live.submission.is_some())
            })
        {
            return;
        }
        let (Ok(turn), Ok(item)) = (native.turn_id.parse(), native.item_id.parse()) else {
            return;
        };
        let mut seen = HashSet::new();
        let mut questions = Vec::with_capacity(native.questions.len());
        for question in native.questions {
            if !visible(&question.id, 256, false)
                || !seen.insert(question.id.clone())
                || !visible(&question.header, 1024, false)
                || !visible(&question.question, 8192, true)
            {
                return;
            }
            let options = question.options.unwrap_or_default();
            if options.len() > MAX_OPTIONS {
                return;
            }
            let mut seen = HashSet::new();
            let mut projected = Vec::with_capacity(options.len());
            for option in options {
                if !visible(&option.label, 1024, false)
                    || !seen.insert(option.label.clone())
                    || option.description.len() > 4096
                {
                    return;
                }
                projected.push(QuestionOption {
                    label: option.label,
                    description: option.description,
                });
            }
            questions.push(Question {
                id: question.id,
                header: question.header,
                prompt: question.question,
                options: projected,
                allows_other: question.is_other,
                secret: question.is_secret,
            });
        }
        let mut digest = Sha256::new();
        for field in [&native.thread_id, &native.turn_id, &native.item_id] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
        let request = ReportHash::from_sha256(digest.finalize().into()).to_string();
        if self.0.get(&request).is_some_and(|prior| {
            prior.form.state != QuestionStatus::Unavailable || prior.submission.is_some()
        }) {
            return;
        }
        self.0.insert(
            request.clone(),
            Live {
                thread: native.thread_id,
                native_id,
                seen: Instant::now(),
                submission: None,
                form: QuestionForm {
                    request,
                    turn,
                    item,
                    blocking: native.is_blocking,
                    state: QuestionStatus::Pending,
                    questions,
                },
            },
        );
    }

    pub(super) fn list(&mut self, thread: &str, request: Option<&str>) -> Vec<QuestionForm> {
        self.prune();
        let mut results: Vec<_> = self
            .0
            .values()
            .filter(|live| {
                live.thread == thread && request.is_none_or(|request| live.form.request == request)
            })
            .collect();
        results.sort_unstable_by_key(|live| {
            (
                !matches!(
                    live.form.state,
                    QuestionStatus::Pending | QuestionStatus::Submitting
                ),
                std::cmp::Reverse(live.seen),
            )
        });
        results
            .into_iter()
            .take(8)
            .map(|live| live.form.clone())
            .collect()
    }

    pub(super) fn prepare(
        &mut self,
        answer: QuestionAnswer,
        expected: QuestionForm,
    ) -> Result<(QuestionForm, Option<Value>), AppError> {
        self.prune();
        let live = self
            .0
            .get_mut(&answer.request)
            .filter(|live| live.thread == answer.identity().native_id().as_str())
            .ok_or(AppError::NotFound)?;
        if live.form.state != QuestionStatus::Pending {
            return Ok((live.form.clone(), None));
        }
        if live.form != expected || live.submission.is_some() {
            return Err(AppError::Conflict);
        }
        answer.validate(&live.form)?;
        live.submission = Some(Submission {
            expires: Instant::now() + Duration::from_secs(MAX_RESULT_SECONDS),
        });
        live.form.state = QuestionStatus::Submitting;
        let answers = answer
            .answers
            .into_iter()
            .map(|(id, values)| (id, json!({"answers":values})))
            .collect::<serde_json::Map<_, _>>();
        Ok((
            live.form.clone(),
            Some(json!({"id":live.native_id, "result":{"answers":answers}})),
        ))
    }

    pub(super) fn disconnect(&mut self) {
        for live in self.0.values_mut() {
            live.form.state = match live.form.state {
                QuestionStatus::Pending => QuestionStatus::Unavailable,
                QuestionStatus::Submitting => QuestionStatus::Uncertain,
                existing => existing,
            };
        }
    }

    pub(super) fn next(&mut self) -> (bool, Option<tokio::time::Instant>) {
        self.prune();
        let expiration = self
            .0
            .values()
            .filter_map(|live| match live.form.state {
                QuestionStatus::Pending => Some(live.seen + CACHE_LIFETIME),
                QuestionStatus::Submitting => live.submission.as_ref().map(|value| value.expires),
                _ => None,
            })
            .min()
            .map(tokio::time::Instant::from_std);
        (expiration.is_some(), expiration)
    }

    fn prune(&mut self) {
        let now = Instant::now();
        self.0.retain(|_, live| now - live.seen <= CACHE_LIFETIME);
        for live in self.0.values_mut() {
            if live.form.state == QuestionStatus::Submitting
                && live
                    .submission
                    .as_ref()
                    .is_some_and(|value| value.expires <= now)
            {
                live.form.state = QuestionStatus::Uncertain;
            }
        }
    }
}

fn valid_native_id(value: &Value) -> bool {
    value.as_i64().is_some() || value.as_str().is_some_and(|id| visible(id, 256, false))
}

fn visible(text: &str, limit: usize, multiline: bool) -> bool {
    !text.is_empty()
        && text.len() <= limit
        && !text
            .chars()
            .any(|ch| ch.is_control() && !(multiline && matches!(ch, '\n' | '\r' | '\t')))
}
