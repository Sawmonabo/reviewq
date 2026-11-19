use reviewq_app::access::AppError;
use reviewq_app::hosts::{
    DeliveryObservation, DispatchGuard, MessageBody, ObservedInput, ReportRef,
};
use reviewq_app::workflow::WorkflowReadiness;
use reviewq_domain::identity::DeliveryId;
use reviewq_domain::session::{HostProvider, LeadTarget};
use serde_json::json;

use super::Codex;
use super::connection::CallError;
use super::history::Item;
use super::session::{ThreadRead, ThreadStatus, canonical_cwd};

impl Codex {
    pub(super) async fn send_to_thread(
        &self,
        lead: LeadTarget,
        delivery: DeliveryId,
        text: MessageBody,
        report: Option<ReportRef>,
        guard: DispatchGuard,
    ) -> DeliveryObservation {
        let automatic = guard.workflow().is_some();
        let head = if automatic {
            match guard.verify_workflow_source().await {
                Ok(head) => Some(head),
                Err(AppError::Conflict | AppError::StaleRevision | AppError::Forbidden) => {
                    return DeliveryObservation::Rejected;
                }
                Err(_) => return DeliveryObservation::NotSubmitted,
            }
        } else {
            None
        };
        let Ok(mut thread) = self.verified_thread(&lead).await else {
            return DeliveryObservation::NotSubmitted;
        };
        if matches!(thread.status, ThreadStatus::NotLoaded) {
            if automatic {
                return DeliveryObservation::NotSubmitted;
            }
            let resumed: Result<ThreadRead, _> = self
                .connection
                .read(
                    "thread/resume",
                    json!({"threadId":thread.id,"excludeTurns":true}),
                )
                .await;
            let Ok(resumed) = resumed else {
                return DeliveryObservation::NotSubmitted;
            };
            let cwd = canonical_cwd(resumed.thread.cwd.clone()).await;
            if resumed.thread.id != thread.id || cwd.as_ref().ok() != Some(lead.binding().cwd()) {
                return DeliveryObservation::NotSubmitted;
            }
            thread = resumed.thread;
        }
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
            let Ok(WorkflowReadiness::Ready(ready)) = self.idle_readiness(&lead, &thread).await
            else {
                return DeliveryObservation::NotSubmitted;
            };
            let Some(previous) = &ready.latest_user_turn_id else {
                return DeliveryObservation::NotSubmitted;
            };
            let Ok(check) = self.turns(&thread.id, None, false, 1, false).await else {
                return DeliveryObservation::NotSubmitted;
            };
            if check
                .data
                .first()
                .is_none_or(|latest| latest.id != previous.as_str() || latest.status != "completed")
                || !self
                    .verified_thread(&lead)
                    .await
                    .is_ok_and(|thread| matches!(thread.status, ThreadStatus::Idle))
            {
                return DeliveryObservation::NotSubmitted;
            }
            let Ok(guard) = guard.confirm_workflow(&ready, &head) else {
                return DeliveryObservation::Rejected;
            };
            params["model"] = json!(ready.model);
            params["effort"] = json!(ready.effort);
            params["turnTrigger"] = json!(turn_marker(&delivery));
            ("turn/start", guard)
        } else {
            let method = match thread.status {
                ThreadStatus::Active { active_flags } if active_flags.is_empty() => {
                    if self.turn_settings.as_ref().is_some_and(|(model, effort)| {
                        thread.model.as_ref() != Some(model)
                            || thread.reasoning_effort.as_ref() != Some(effort)
                    }) {
                        return DeliveryObservation::NotSubmitted;
                    }
                    let Ok(mut turns) = self.turns(&thread.id, None, false, 1, false).await else {
                        return DeliveryObservation::NotSubmitted;
                    };
                    let Some(turn) = turns.data.pop().filter(|row| row.status == "inProgress")
                    else {
                        return DeliveryObservation::NotSubmitted;
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
                _ => return DeliveryObservation::NotSubmitted,
            };
            (method, guard)
        };

        match self.connection.submit(method, params, guard).await {
            Err(CallError::BeforeSend(_)) => DeliveryObservation::NotSubmitted,
            Err(CallError::Denied(_)) => DeliveryObservation::Rejected,
            // A reply, missing reply or exact remote error alone does not prove native input.
            _ => self.find_input(lead, delivery, None, automatic).await,
        }
    }

    pub(super) async fn find_input(
        &self,
        lead: LeadTarget,
        delivery: DeliveryId,
        mut cursor: Option<String>,
        require_new_turn: bool,
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
        for _ in 0..4 {
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
