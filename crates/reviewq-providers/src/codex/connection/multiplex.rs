//! The loop that owns one daemon session's socket. It writes the requests callers queue,
//! hands each reply frame to its caller and every other frame to the question and live
//! caches. A request with a guard is checked beside that work, never in its way: a check reads
//! the store or a transcript and waits its turn there, while the loop keeps reading frames and
//! serving other requests, and the request is written the moment its check passes. A request
//! is written only by its `write_by` and only while its caller has not withdrawn it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use reviewq_app::access::AppError;
use reviewq_app::hosts::{QuestionAnswer, QuestionForm};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout, timeout_at};

use super::super::live::Live;
use super::super::questions::Questions;
use super::{
    AnswerError, AnswerRequest, CHECK_LIMIT, CallError, CommandRequest, DEADLINE, IDLE_TIME, Mark,
    Pending, REQUEST_LIMIT, Rpc, Served, Socket, receive_frame, send,
};

/// A request the loop took and has not written, while its guard is checked. Kept apart from
/// the check, so a loop that ends first still tells its caller that nothing was sent.
enum Held {
    Rpc(Box<Rpc>),
    Answer {
        form: QuestionForm,
        write_by: Instant,
        mark: Mark,
        reply: oneshot::Sender<Result<QuestionForm, AnswerError>>,
    },
}

/// One guard check in flight. It ends with the key of its held request, the answer to send
/// when there is one, and what the guard said, or `None` when the request's `write_by` came
/// first.
type Check<'a> = BoxFuture<'a, (u64, Option<QuestionAnswer>, Option<Result<(), AppError>>)>;

/// The loop's state for one daemon session.
struct Work<'a> {
    socket: &'a mut Socket,
    questions: &'a Mutex<Questions>,
    id: &'a mut u64,
    /// Written requests waiting for their reply, by request ID, each until `DEADLINE` after
    /// it was written.
    pending: HashMap<u64, Pending>,
    held: HashMap<u64, Held>,
    checks: FuturesUnordered<Check<'a>>,
    next_key: u64,
}

/// Serves one daemon session from its `first` request until the socket fails, every
/// `Connection` is gone, or it has been idle for `IDLE_TIME` with nothing live. Every request
/// it took is answered: one still waiting for its reply as `Uncertain`, and one never written
/// as `BeforeSend`.
pub(super) async fn run(
    socket: &mut Socket,
    first: Rpc,
    commands: &mut mpsc::Receiver<CommandRequest>,
    questions: &Mutex<Questions>,
    live: &Live,
    session: &Arc<Served>,
    id: &mut u64,
) {
    let mut work = Work {
        socket,
        questions,
        id,
        pending: HashMap::new(),
        held: HashMap::new(),
        checks: FuturesUnordered::new(),
        next_key: 0,
    };
    if work.take(CommandRequest::Rpc(first)).await.is_ok() {
        work.serve(commands, live, session).await;
    }
    work.end();
}

impl<'a> Work<'a> {
    async fn serve(
        &mut self,
        commands: &mut mpsc::Receiver<CommandRequest>,
        live: &Live,
        session: &Arc<Served>,
    ) {
        let mut last_activity = Instant::now();
        loop {
            let (question_active, question_deadline) = self
                .questions
                .lock()
                .ok()
                .map(|mut cache| cache.next())
                .unwrap_or((false, None));
            let next_deadline = self
                .pending
                .values()
                .map(|item| item.deadline)
                .min()
                .or_else(|| (!question_active).then_some(last_activity + IDLE_TIME));
            let wake = match (next_deadline, question_deadline) {
                (Some(left), Some(right)) => left.min(right),
                (Some(value), None) | (None, Some(value)) => value,
                (None, None) => last_activity + IDLE_TIME,
            };
            // A check counts apart from the requests waiting for a reply, so slow checks
            // never keep a plain request from being written.
            let room = self.pending.len() < REQUEST_LIMIT && self.held.len() < CHECK_LIMIT;
            let checking = !self.checks.is_empty();
            tokio::select! {
                work = commands.recv(), if room => {
                    last_activity = Instant::now();
                    let Some(work) = work else { break };
                    if self.take(work).await.is_err() { break; }
                }
                Some((key, answer, checked)) = self.checks.next(), if checking => {
                    last_activity = Instant::now();
                    if self.checked(key, answer, checked).await.is_err() { break; }
                }
                event = receive_frame(self.socket) => {
                    last_activity = Instant::now();
                    match event {
                        Ok(Some(frame)) if frame.get("method").is_some() => {
                            if let Ok(mut cache) = self.questions.lock() { cache.frames(&frame); }
                            live.frame(&frame);
                        }
                        Ok(Some(frame)) => {
                            if let Some(id) = frame.get("id").and_then(Value::as_u64)
                                && let Some(active) = self.pending.remove(&id) {
                                let _ = active.reply.send(Ok((frame, session.clone())));
                            }
                        }
                        Ok(None) => (),
                        Err(()) => break,
                    }
                }
                () = sleep_until(wake) => {
                    let now = Instant::now();
                    self.pending.retain(|_, item| !item.reply.is_closed() && item.deadline > now);
                    if self.pending.is_empty() && self.held.is_empty() && !question_active
                        && now >= last_activity + IDLE_TIME
                    {
                        if !live.active() { break; }
                        last_activity = now;
                    }
                }
            }
        }
    }

    /// Starts on one queued request. One with a guard is held while the guard is checked,
    /// as is every answer, until the request's `write_by`; any other is written at once.
    /// `Err` when a write failed, which ends the session.
    async fn take(&mut self, work: CommandRequest) -> Result<(), ()> {
        let key = self.next_key;
        let questions = self.questions;
        let check: Check<'a> = match work {
            CommandRequest::Rpc(mut request) => {
                if request.reply.is_closed() {
                    return Ok(());
                }
                let Some(guard) = request.guard.take() else {
                    return self.write(request).await;
                };
                let write_by = request.write_by;
                self.held.insert(key, Held::Rpc(Box::new(request)));
                Box::pin(async move {
                    let checked = timeout_at(write_by, guard.check(questions)).await;
                    (key, None, checked.ok())
                })
            }
            CommandRequest::Answer(AnswerRequest {
                answer,
                form,
                write_by,
                mark,
                reply,
            }) => {
                if reply.is_closed() {
                    return Ok(());
                }
                let held = Held::Answer {
                    form,
                    write_by,
                    mark,
                    reply,
                };
                self.held.insert(key, held);
                Box::pin(async move {
                    let checked = timeout_at(write_by, answer.check()).await;
                    (key, Some(answer), checked.ok())
                })
            }
        };
        self.next_key = key.wrapping_add(1);
        self.checks.push(check);
        Ok(())
    }

    /// Finishes the held request `key` once its check ended: writes it when the check
    /// passed, and otherwise tells its caller that nothing was sent, `Denied` when the guard
    /// refused and `BeforeSend` with `Busy` when its `write_by` came first.
    async fn checked(
        &mut self,
        key: u64,
        answer: Option<QuestionAnswer>,
        checked: Option<Result<(), AppError>>,
    ) -> Result<(), ()> {
        match self.held.remove(&key) {
            Some(Held::Rpc(request)) => match checked {
                Some(Ok(())) => self.write(*request).await,
                Some(Err(error)) => {
                    let _ = request.reply.send(Err(CallError::Denied(error)));
                    Ok(())
                }
                None => {
                    let _ = request
                        .reply
                        .send(Err(CallError::BeforeSend(AppError::Busy)));
                    Ok(())
                }
            },
            Some(Held::Answer {
                form,
                write_by,
                mark,
                reply,
            }) => match (answer, checked) {
                (Some(answer), Some(Ok(()))) => {
                    self.answer(answer, form, write_by, &mark, reply).await
                }
                _ => {
                    let _ = reply.send(Err(AnswerError::BeforeSend));
                    Ok(())
                }
            },
            None => Ok(()),
        }
    }

    /// Writes `request` and waits for its reply until `DEADLINE` after the write, unless its
    /// `write_by` passed or its caller withdrew it. `Err` when the write failed, which leaves
    /// the request `Uncertain` and ends the session.
    async fn write(&mut self, request: Rpc) -> Result<(), ()> {
        if request.reply.is_closed() {
            return Ok(());
        }
        if Instant::now() >= request.write_by {
            let _ = request
                .reply
                .send(Err(CallError::BeforeSend(AppError::Busy)));
            return Ok(());
        }
        if !request.mark.write() {
            return Ok(());
        }
        *self.id = self.id.wrapping_add(1).max(2);
        let id = *self.id;
        let wire = json!({"id": id, "method": request.method, "params": request.params});
        if !matches!(timeout(DEADLINE, send(self.socket, wire)).await, Ok(Ok(()))) {
            let _ = request.reply.send(Err(CallError::Uncertain));
            return Err(());
        }
        self.pending.insert(
            id,
            Pending {
                reply: request.reply,
                deadline: Instant::now() + DEADLINE,
            },
        );
        Ok(())
    }

    /// Sends an answer whose check passed, when the question cache still has the question
    /// open for it, its `write_by` has not passed and its caller has not withdrawn it. `Err`
    /// when the write failed, which ends the session.
    async fn answer(
        &mut self,
        answer: QuestionAnswer,
        form: QuestionForm,
        write_by: Instant,
        mark: &Mark,
        reply: oneshot::Sender<Result<QuestionForm, AnswerError>>,
    ) -> Result<(), ()> {
        if reply.is_closed() {
            return Ok(());
        }
        if Instant::now() >= write_by {
            let _ = reply.send(Err(AnswerError::BeforeSend));
            return Ok(());
        }
        if !mark.write() {
            return Ok(());
        }
        let prepared = self
            .questions
            .lock()
            .map_err(|_| AppError::Internal)
            .and_then(|mut cache| cache.prepare(answer, form));
        let result = match prepared {
            Ok((form, Some(outbound))) => {
                if !matches!(
                    timeout(DEADLINE, send(self.socket, outbound)).await,
                    Ok(Ok(()))
                ) {
                    let _ = reply.send(Err(AnswerError::Uncertain));
                    return Err(());
                }
                Ok(form)
            }
            Ok((form, None)) => Ok(form),
            Err(_) => Err(AnswerError::BeforeSend),
        };
        let _ = reply.send(result);
        Ok(())
    }

    /// Answers every request the session still has: `Uncertain` for one written, whose reply
    /// did not come, and `BeforeSend` for one held for its check.
    fn end(self) {
        for (_, item) in self.pending {
            let _ = item.reply.send(Err(CallError::Uncertain));
        }
        for (_, held) in self.held {
            match held {
                Held::Rpc(request) => {
                    let _ = request
                        .reply
                        .send(Err(CallError::BeforeSend(AppError::Unavailable)));
                }
                Held::Answer { reply, .. } => {
                    let _ = reply.send(Err(AnswerError::BeforeSend));
                }
            }
        }
    }
}
