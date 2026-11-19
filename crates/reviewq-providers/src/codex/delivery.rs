use reviewq_app::hosts::{
    DeliveryObservation, DispatchGuard, MessageBody, ObservedInput, ReportRef,
};
use reviewq_domain::identity::DeliveryId;
use reviewq_domain::session::LeadTarget;
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
        let Ok(mut thread) = self.verified_thread(&lead).await else {
            return DeliveryObservation::NotSubmitted;
        };
        if matches!(thread.status, ThreadStatus::NotLoaded) {
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
                let Some(turn) = turns.data.pop().filter(|row| row.status == "inProgress") else {
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

        match self.connection.submit(method, params, guard).await {
            Err(CallError::BeforeSend(_)) => DeliveryObservation::NotSubmitted,
            Err(CallError::Denied(_)) => DeliveryObservation::Rejected,
            // A reply, missing reply or exact remote error alone does not prove native input.
            _ => self.find_input(lead, delivery).await,
        }
    }

    pub(super) async fn find_input(
        &self,
        lead: LeadTarget,
        delivery: DeliveryId,
    ) -> DeliveryObservation {
        let Ok(thread) = self.verified_thread(&lead).await else {
            return DeliveryObservation::Uncertain;
        };
        let Ok(turns) = self.turns(&thread.id, None, false, 8, true).await else {
            return DeliveryObservation::Uncertain;
        };
        for turn in turns.data {
            for item in turn.items {
                if let Item::UserMessage { id, client_id, .. } = item
                    && client_id.as_deref() == Some(delivery.as_str())
                    && let (Ok(turn_id), Ok(entry_id)) = (turn.id.parse(), id.parse())
                {
                    return DeliveryObservation::Started {
                        delivery_id: delivery,
                        input: ObservedInput { turn_id, entry_id },
                    };
                }
            }
        }
        DeliveryObservation::Uncertain
    }
}
