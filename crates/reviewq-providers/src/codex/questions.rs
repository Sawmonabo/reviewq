use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{
    Question, QuestionAnswer, QuestionForm, QuestionOption, QuestionStatus, QuestionsPort,
};
use reviewq_domain::identity::SubmissionId;
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Codex;

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
    id: SubmissionId,
    digest: [u8; 32],
    expires: Instant,
}

pub(super) struct AnswerProof {
    pub request: String,
    pub item: String,
    pub digest: [u8; 32],
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
        session: HistorySession,
        request: Option<String>,
    ) -> AppFuture<'_, Vec<QuestionForm>> {
        Box::pin(async move {
            let native = session.binding.identity();
            if native.profile() != &self.profile || native.provider() != HostProvider::Codex {
                return Err(AppError::NotFound);
            }
            let thread = native.native_id().as_str();
            let pending = self
                .connection
                .question_proofs(thread, request.as_deref())?;
            if !pending.is_empty()
                && let Ok(_permit) = self.question_proofs.try_acquire()
                && let Ok(observed) = self.read_thread(native.native_id()).await
                && let Some(path) = observed.path
            {
                for proof in pending {
                    if crate::history::codex_question_answered(
                        session.binding.clone(),
                        path.clone(),
                        proof.item.clone(),
                        proof.digest,
                    )
                    .await
                    .unwrap_or(false)
                    {
                        self.connection.question_answered(thread, proof)?;
                    }
                }
            }
            self.connection.questions(thread, request.as_deref())
        })
    }

    fn answer_question(&self, answer: QuestionAnswer) -> AppFuture<'_, QuestionForm> {
        Box::pin(async move {
            if answer.identity().profile() != &self.profile
                || answer.identity().provider() != HostProvider::Codex
            {
                return Err(AppError::NotFound);
            }
            let forms = self.connection.questions(
                answer.identity().native_id().as_str(),
                Some(&answer.request),
            )?;
            let form = forms.first().ok_or(AppError::NotFound)?;
            if form.state == QuestionStatus::Pending {
                let _permit = self
                    .question_proofs
                    .try_acquire()
                    .map_err(|_| AppError::Busy)?;
                let session = answer.session();
                if session.parent.is_some() {
                    return Err(AppError::Unavailable);
                }
                let native = self.read_thread(answer.identity().native_id()).await?;
                let path = native.path.ok_or(AppError::Unavailable)?;
                crate::history::codex_question_opening(
                    session.binding.clone(),
                    path,
                    form.item.as_str().to_owned(),
                )
                .await?;
            }
            self.connection.answer(answer).await
        })
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
            || self
                .0
                .values()
                .any(|live| live.thread == native.thread_id && live.native_id == native_id)
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
        let request = Uuid::new_v4().to_string();
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
    ) -> Result<(QuestionForm, Option<Value>), AppError> {
        self.prune();
        let live = self
            .0
            .get_mut(&answer.request)
            .filter(|live| live.thread == answer.identity().native_id().as_str())
            .ok_or(AppError::NotFound)?;
        let encoded = serde_json::to_vec(&answer.answers).map_err(|_| AppError::InvalidRequest)?;
        if encoded.len() > FRAME_BUDGET {
            return Err(AppError::PayloadTooLarge);
        }
        let digest: [u8; 32] = Sha256::digest(encoded).into();
        if let Some(prior) = &live.submission {
            if prior.id != answer.submission || prior.digest != digest {
                return Err(AppError::Conflict);
            }
            return Ok((live.form.clone(), None));
        }
        if live.form.state != QuestionStatus::Pending {
            return Ok((live.form.clone(), None));
        }
        if ((!live.form.blocking || !answer.allows_secret())
            && live.form.questions.iter().any(|question| question.secret))
            || answer.answers.len() != live.form.questions.len()
        {
            return Err(AppError::InvalidRequest);
        }
        for question in &live.form.questions {
            let Some([value]) = answer.answers.get(&question.id).map(Vec::as_slice) else {
                return Err(AppError::InvalidRequest);
            };
            if !visible(value, 8192, true)
                || (!question.allows_other
                    && !question.options.is_empty()
                    && !question.options.iter().any(|option| &option.label == value))
            {
                return Err(AppError::InvalidRequest);
            }
        }
        live.submission = Some(Submission {
            id: answer.submission,
            digest,
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

    pub(super) fn proofs(&self, thread: &str, request: Option<&str>) -> Vec<AnswerProof> {
        let now = Instant::now();
        self.0
            .values()
            .filter_map(|live| {
                let submission = live.submission.as_ref()?;
                if live.thread != thread
                    || live.form.state != QuestionStatus::Closed
                    || request.is_some_and(|request| live.form.request != request)
                    || now > submission.expires + Duration::from_secs(25)
                {
                    return None;
                }
                Some(AnswerProof {
                    request: live.form.request.clone(),
                    item: live.form.item.as_str().to_owned(),
                    digest: submission.digest,
                })
            })
            .take(2)
            .collect()
    }

    pub(super) fn confirmed(&mut self, thread: &str, proof: AnswerProof) {
        if let Some(live) = self.0.get_mut(&proof.request)
            && live.thread == thread
            && live.form.item.as_str() == proof.item
            && live
                .submission
                .as_ref()
                .is_some_and(|submission| submission.digest == proof.digest)
        {
            live.form.state = QuestionStatus::Answered;
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
