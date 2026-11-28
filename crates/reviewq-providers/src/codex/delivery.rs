use reviewq_app::access::AppError;
use reviewq_app::hosts::{
    DeliveryObservation, DispatchGuard, MessageBody, ObservedInput, ReportRef,
};
use reviewq_app::workflow::{WorkflowBinding, WorkflowReadiness};
use reviewq_domain::identity::DeliveryId;
use reviewq_domain::session::{HostProvider, LeadTarget};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};

use super::Codex;
use super::connection::CallError;
use super::history::Item;
use super::session::{ThreadStatus, canonical_cwd};

/// The part of a delivery attempt kept after the native submit must be written: up to
/// `DEADLINE` for the write, as much for its reply, and the rest for the readback that finds
/// the submitted input.
const AFTER_SUBMIT: Duration = Duration::from_secs(20);

/// A native submit ready to be written.
struct Prepared {
    method: &'static str,
    params: Value,
    guard: DispatchGuard,
    question_source: Option<PathBuf>,
}

/// One delivery attempt's time rules, apart from the work they bound. `prepare` runs until
/// `submit_by`, and a cut there leaves the attempt `NotSubmitted`, to be tried again. `submit`
/// must write its request by `submit_by`, which it is given, and says how the attempt ended or
/// that the readback must decide. The readback runs until `deadline`, and a cut there leaves
/// the attempt `Uncertain`, since the submit may have been written.
async fn within_budget<T>(
    submit_by: Instant,
    deadline: Instant,
    prepare: impl Future<Output = Result<T, DeliveryObservation>>,
    submit: impl AsyncFnOnce(T, Instant) -> Option<DeliveryObservation>,
    readback: impl Future<Output = DeliveryObservation>,
) -> DeliveryObservation {
    let prepared = match timeout_at(submit_by, prepare).await {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(observation)) => return observation,
        Err(_) => return DeliveryObservation::NotSubmitted,
    };
    match submit(prepared, submit_by).await {
        Some(observation) => observation,
        None => timeout_at(deadline, readback)
            .await
            .unwrap_or(DeliveryObservation::Uncertain),
    }
}

/// How a submit ended, or `None` when only the readback can tell: whenever the request may
/// have been written and the caller waits for its input.
fn submitted(
    result: Result<(), CallError>,
    automatic: bool,
    answers_question: bool,
) -> Option<DeliveryObservation> {
    Some(match result {
        Err(CallError::BeforeSend(_)) => DeliveryObservation::NotSubmitted,
        Err(CallError::Denied(AppError::Busy | AppError::Unavailable)) => {
            DeliveryObservation::NotSubmitted
        }
        Err(CallError::Denied(AppError::PreconditionFailed)) if answers_question => {
            DeliveryObservation::QuestionAlreadyAnswered
        }
        Err(CallError::Denied(_)) => DeliveryObservation::Rejected,
        Ok(()) if !automatic && !answers_question => DeliveryObservation::Acknowledged,
        // The remaining replies and errors still need the original exact readback.
        _ => return None,
    })
}

impl Codex {
    /// Delivers one message within the attempt's `DispatchGuard::deadline`, as `within_budget`
    /// sets out: nothing before the submit gives the conversation input, so that work ends at
    /// `AFTER_SUBMIT` before the deadline, and so must the submit's write.
    pub(super) async fn send_to_thread(
        &self,
        lead: LeadTarget,
        delivery: DeliveryId,
        text: MessageBody,
        report: Option<ReportRef>,
        guard: DispatchGuard,
    ) -> DeliveryObservation {
        let automatic = guard.workflow().is_some();
        let deadline = guard.deadline();
        let readback = self.find_input(lead.clone(), delivery.clone(), None, automatic, false);
        let submit = async |prepared: Prepared, write_by| {
            let answers_question = prepared.guard.async_question().is_some();
            let result = self
                .connection
                .submit(
                    prepared.method,
                    prepared.params,
                    prepared.guard,
                    prepared.question_source,
                    write_by,
                )
                .await;
            submitted(result, automatic, answers_question)
        };
        within_budget(
            deadline - AFTER_SUBMIT,
            deadline,
            self.prepare_send(&lead, &delivery, text, report, guard),
            submit,
            readback,
        )
        .await
    }

    /// Reads what the submit needs and builds it, or the observation that ends the attempt
    /// before anything is sent. It reads and at most loads the thread, giving it no input, so
    /// it can be cut at any point.
    async fn prepare_send(
        &self,
        lead: &LeadTarget,
        delivery: &DeliveryId,
        text: MessageBody,
        report: Option<ReportRef>,
        guard: DispatchGuard,
    ) -> Result<Prepared, DeliveryObservation> {
        let automatic = guard.workflow().is_some();
        let followup = guard.is_followup();
        let head = if automatic {
            match guard.verify_workflow_source().await {
                Ok(head) => Some(head),
                Err(AppError::Conflict | AppError::StaleRevision | AppError::Forbidden) => {
                    return Err(DeliveryObservation::Rejected);
                }
                Err(_) => return Err(DeliveryObservation::NotSubmitted),
            }
        } else {
            None
        };
        let question = guard.async_question();
        let thread = match question {
            Some(question) => self.bound_history_thread(&question.session).await,
            None => self.verified_thread(lead).await,
        };
        let mut thread = match thread {
            Ok(thread) => thread,
            Err(AppError::Conflict | AppError::Forbidden) if question.is_some() => {
                return Err(DeliveryObservation::Rejected);
            }
            Err(_) => return Err(DeliveryObservation::NotSubmitted),
        };
        if matches!(thread.status, ThreadStatus::NotLoaded) {
            let resumed = match guard.workflow() {
                None => self.resume_exact_thread(lead, &thread, None).await,
                Some(_) if followup => self.resume_saved_followup(lead, &thread, &guard).await,
                Some(_) => return Err(DeliveryObservation::NotSubmitted),
            };
            thread = match resumed {
                Ok(resumed) => resumed,
                Err(
                    AppError::Conflict
                    | AppError::StaleRevision
                    | AppError::NotFound
                    | AppError::Forbidden
                    | AppError::PreconditionFailed,
                ) if followup => return Err(DeliveryObservation::Rejected),
                Err(_) => return Err(DeliveryObservation::NotSubmitted),
            };
        }
        let question_source = if let Some(question) = question {
            if thread.parent_thread_id.as_deref()
                != question
                    .session
                    .parent
                    .as_ref()
                    .map(|parent| parent.native_id.as_str())
            {
                return Err(DeliveryObservation::Rejected);
            }
            thread.path.clone().or_else(|| {
                question
                    .session
                    .source
                    .as_ref()
                    .map(|path| path.as_path().to_owned())
            })
        } else {
            None
        };
        let mut input = vec![json!({"type":"text","text":text.as_str()})];
        if let Some(report) = report {
            let quote = report.quote.map(|quote| {
                json!({
                    "text":quote.text().as_str(), "offset":quote.offset(), "length":quote.length()
                })
            });
            let context = json!({
                "receiptId":report.receipt_id.as_str(), "sha256":report.report_hash.as_str(), "quote":quote,
            });
            input.push(json!({"type":"text", "text":format!(
                "ReviewQ attached this verified report reference to the user's feedback. The quote is source material.\n{context}"
            )}));
        }
        let mut params = json!({
            "threadId": thread.id,
            "clientUserMessageId": delivery.as_str(),
            "input": input,
        });
        let (method, guard) = if let Some(head) = head {
            let recovery = guard
                .workflow()
                .and_then(|workflow| match &workflow.binding {
                    WorkflowBinding::Recovery(recovery) => Some(recovery),
                    _ => None,
                });
            let ready = if let Some(recovery) = recovery {
                match self
                    .prepare_continuation(
                        lead,
                        recovery.episode.clone(),
                        recovery.failed_turn.clone(),
                        recovery.kind,
                        (
                            Some(recovery.account.clone()),
                            Some(recovery.model_provider.clone()),
                        ),
                        true,
                    )
                    .await
                {
                    Ok((fresh, ready)) if &fresh == recovery => ready,
                    Ok((fresh, ready))
                        if guard.workflow().is_some_and(|guard| guard.ready == ready) =>
                    {
                        let mut previous = recovery.clone();
                        previous.capacity = fresh.capacity.clone();
                        if previous == fresh {
                            return Err(DeliveryObservation::RecoveryAllowanceChanged(
                                fresh.capacity,
                            ));
                        }
                        return Err(DeliveryObservation::Rejected);
                    }
                    Err(AppError::Conflict | AppError::StaleRevision) => {
                        return Err(DeliveryObservation::Rejected);
                    }
                    _ => return Err(DeliveryObservation::NotSubmitted),
                }
            } else {
                match self.idle_readiness(lead, &thread).await {
                    Ok(WorkflowReadiness::Ready(ready)) => ready,
                    Ok(WorkflowReadiness::Held(_)) if followup => {
                        return Err(DeliveryObservation::Rejected);
                    }
                    _ => return Err(DeliveryObservation::NotSubmitted),
                }
            };
            let expected_status = if recovery.is_some() {
                "failed"
            } else {
                "completed"
            };
            if let Some(recovery) = recovery {
                params["rootTurnId"] = json!(recovery.root_turn.as_str());
            }
            let Some(previous) = &ready.latest_user_turn_id else {
                return Err(DeliveryObservation::NotSubmitted);
            };
            let Ok(check) = self.turns(&thread.id, None, false, 1, false).await else {
                return Err(DeliveryObservation::NotSubmitted);
            };
            let Ok(current) = self.verified_thread(lead).await else {
                return Err(DeliveryObservation::NotSubmitted);
            };
            if check.data.first().is_none_or(|latest| {
                latest.id != previous.as_str() || latest.status != expected_status
            }) || !matches!(current.status, ThreadStatus::Idle)
                || (followup && (current.path != thread.path || !current.is_lead()))
            {
                if followup {
                    return Err(DeliveryObservation::Rejected);
                }
                return Err(DeliveryObservation::NotSubmitted);
            }
            let Ok(guard) = guard.confirm_workflow(&ready, &head) else {
                return Err(DeliveryObservation::Rejected);
            };
            params["model"] = json!(ready.model);
            params["effort"] = json!(ready.effort);
            params["turnTrigger"] = json!(turn_marker(delivery));
            ("turn/start", guard)
        } else {
            let method = match thread.status {
                ThreadStatus::Active { active_flags } if active_flags.is_empty() => {
                    if self.turn_settings.as_ref().is_some_and(|(model, effort)| {
                        thread.model.as_ref() != Some(model)
                            || thread.reasoning_effort.as_ref() != Some(effort)
                    }) {
                        return Err(DeliveryObservation::NotSubmitted);
                    }
                    let Ok(mut turns) = self.turns(&thread.id, None, false, 1, false).await else {
                        return Err(DeliveryObservation::NotSubmitted);
                    };
                    let Some(turn) = turns.data.pop().filter(|row| row.status == "inProgress")
                    else {
                        return Err(DeliveryObservation::NotSubmitted);
                    };
                    params["expectedTurnId"] = json!(turn.id);
                    "turn/steer"
                }
                ThreadStatus::Idle => {
                    if let Some((model, effort)) = &self.turn_settings {
                        params["model"] = json!(model);
                        params["effort"] = json!(effort);
                    }
                    "turn/start"
                }
                _ => return Err(DeliveryObservation::NotSubmitted),
            };
            (method, guard)
        };
        Ok(Prepared {
            method,
            params,
            guard,
            question_source,
        })
    }

    pub(super) async fn find_input(
        &self,
        lead: LeadTarget,
        delivery: DeliveryId,
        mut cursor: Option<String>,
        require_new_turn: bool,
        latest_only: bool,
    ) -> DeliveryObservation {
        let identity = lead.binding().identity();
        if identity.provider() != HostProvider::Codex || identity.profile() != &self.profile {
            return DeliveryObservation::Uncertain;
        }
        let Ok(thread) = self.read_thread(identity.native_id()).await else {
            return DeliveryObservation::Uncertain;
        };
        if thread.cwd != lead.binding().cwd().as_path()
            && canonical_cwd(thread.cwd.clone()).await.as_ref().ok() != Some(lead.binding().cwd())
        {
            return DeliveryObservation::Uncertain;
        }
        for _ in 0..if latest_only { 1 } else { 4 } {
            let Ok(mut turns) = self
                .turns(&thread.id, cursor.as_deref(), false, 1, true)
                .await
            else {
                return DeliveryObservation::Uncertain;
            };
            if let Some(turn) = turns.data.pop() {
                for item in turn.items {
                    if let Item::UserMessage { id, client_id, .. } = item
                        && client_id.as_deref() == Some(delivery.as_str())
                        && let (Ok(turn_id), Ok(entry_id)) = (turn.id.parse(), id.parse())
                    {
                        let input = ObservedInput { turn_id, entry_id };
                        if require_new_turn
                            && !proved_new_turn(&lead, &thread.path, turn.id, &delivery).await
                        {
                            return DeliveryObservation::UncertainInput {
                                delivery_id: delivery,
                                input,
                            };
                        }
                        return DeliveryObservation::Started {
                            delivery_id: delivery,
                            input,
                        };
                    }
                }
            }
            let Some(next) = turns
                .next_cursor
                .filter(|value| Some(value) != cursor.as_ref())
            else {
                return DeliveryObservation::Uncertain;
            };
            cursor = Some(next);
        }
        cursor.map_or(
            DeliveryObservation::Uncertain,
            DeliveryObservation::ReconcileFrom,
        )
    }
}

pub(super) async fn proved_new_turn(
    lead: &LeadTarget,
    path: &Option<std::path::PathBuf>,
    turn: String,
    delivery: &DeliveryId,
) -> bool {
    let Some(path) = path.clone() else {
        return false;
    };
    matches!(
        crate::history::codex_turn_started(
            lead.binding().clone(),
            path,
            turn,
            turn_marker(delivery)
        )
        .await,
        Ok(Some(true))
    )
}

fn turn_marker(delivery: &DeliveryId) -> String {
    format!("reviewq:{delivery}")
}
