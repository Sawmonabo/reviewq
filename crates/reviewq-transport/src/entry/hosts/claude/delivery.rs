use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::hosts::{
    ClaudeChild, ClaudeClassicEvent, ClaudeDeliveryAck, ClaudeDeliveryResult, ClaudeTurnResult,
    DeliveryWork, ObservedInput,
};
use reviewq_app::ledger::{GateRequest, HookSession};
use reviewq_domain::session::AbsolutePath;
use reviewq_protocol::Success;
use reviewq_protocol::ledger::GateReply;
use serde::{Deserialize, Serialize};

use super::{ClaudeCommand, ClaudeOffer, registered_lead, verified_context};
use crate::entry::error::ApiBytes;
use crate::entry::features::ledger::{answer_gate, read_back};
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Offer {
    delivery_id: String,
    fence: u64,
    generation: String,
}

impl Offer {
    pub(super) fn from_offer(value: ClaudeOffer) -> Result<Self, AppError> {
        if value.fence.get() > 9_007_199_254_740_991 {
            return Err(AppError::Internal);
        }
        Ok(Self {
            delivery_id: value.id.to_string(),
            fence: value.fence.get(),
            generation: value.generation,
        })
    }

    fn checked(self) -> Result<ClaudeOffer, AppError> {
        Ok(ClaudeOffer {
            id: parse(&self.delivery_id)?,
            fence: self
                .fence
                .try_into()
                .map_err(|_| AppError::InvalidRequest)?,
            generation: self.generation,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Check {
    context: ClaudeCommand,
    offer: Offer,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Effect {
    text: String,
    report_context: Option<String>,
    settings: Option<Settings>,
}

#[derive(Serialize)]
struct Settings {
    model: String,
    effort: String,
}

pub(super) async fn check(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<Effect>>, ApiError> {
    let command: Check = entry.auth.bridge_command(&headers, &bytes, 16_384)?;
    if !entry.sends_allowed {
        return Err(AppError::Unavailable.into());
    }
    let (lead, session) = verified_context(&entry, command.context).await?;
    let companion = entry.claude.as_ref().ok_or(AppError::Unavailable)?;
    let offer = command.offer.checked()?;
    let pending = entry
        .store
        .claude_delivery(lead.clone(), offer.clone())
        .await?;
    if pending.anchor.is_some() {
        return Err(AppError::Conflict.into());
    }
    let DeliveryWork::Dispatch { workflow, .. } = &pending.claim.work else {
        return Err(AppError::Conflict.into());
    };
    let ready = if let Some(guard) = workflow {
        let current = pending.ready.ok_or(AppError::Unavailable)?;
        if current != guard.ready {
            return Err(AppError::StaleRevision.into());
        }
        let requirements = entry.store.workflow_requirements(&pending.claim).await?;
        let source = entry.source_reader().await?;
        let observed = source
            .workflow_snapshot(requirements.identity.clone())
            .await?;
        requirements.check(&observed, &guard.head)?;
        Some(current)
    } else {
        None
    };
    let anchor = companion.anchor(session).await?;
    let claim = entry
        .store
        .authorize_claude_delivery(lead, offer, anchor, ready)
        .await?;
    let DeliveryWork::Dispatch {
        text,
        report_ref,
        workflow,
        ..
    } = claim.work
    else {
        return Err(AppError::Conflict.into());
    };
    let report_context = report_ref.map(|reference| {
        let mut context = format!(
            "ReviewQ feedback refers to accepted report receipt {} (SHA-256 {}).",
            reference.receipt_id, reference.report_hash
        );
        if let Some(quote) = reference.quote {
            use std::fmt::Write as _;
            let _ = write!(
                context,
                "\nExact displayed source quote at Unicode offset {}:\n{}",
                quote.offset(),
                quote.text().as_str()
            );
        }
        context
    });
    Ok(Json(success(
        Effect {
            text: text.as_str().to_owned(),
            report_context,
            settings: workflow.map(|guard| Settings {
                model: guard.ready.model,
                effort: guard.ready.effort,
            }),
        },
        *entry.commits.borrow(),
    )?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeliveryHook {
    context: ClaudeCommand,
    offer: Offer,
    outcome: Outcome,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TurnHook {
    context: ClaudeCommand,
    generation: String,
    turn_id: String,
    normal: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChildHook {
    context: ClaudeCommand,
    child_id: String,
    generation: String,
    sequence: u64,
    parent_id: Option<String>,
    child_cwd: std::path::PathBuf,
    status: ChildStatus,
    name: Option<String>,
    started_at_ms: Option<i64>,
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ChildStatus {
    Pending,
    Running,
    Waiting,
    Idle,
    Completed,
    Failed,
    Killed,
}

impl ChildStatus {
    fn facts(self) -> (&'static str, reviewq_app::hosts::HostPhase) {
        use reviewq_app::hosts::HostPhase;
        match self {
            Self::Pending => ("pending", HostPhase::Working),
            Self::Running => ("running", HostPhase::Working),
            Self::Waiting => ("waiting", HostPhase::Blocked),
            Self::Idle => ("idle", HostPhase::Idle),
            Self::Completed => ("completed", HostPhase::Idle),
            Self::Failed => ("failed", HostPhase::Failed),
            Self::Killed => ("killed", HostPhase::Idle),
        }
    }
}

fn child_label(value: Option<String>, max: usize) -> Option<String> {
    let value = value?;
    (value.len() <= max && !value.trim().is_empty() && !value.chars().any(char::is_control))
        .then_some(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassicHook {
    context: ClaudeCommand,
    classic: Classic,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Classic {
    PermissionPending,
    Display {
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        index: u64,
        #[serde(rename = "finalChunk")]
        final_chunk: bool,
    },
    /// Claude's `Stop` hook. Answered with the gate's reply; `stopHookActive` is the hook
    /// input's `stop_hook_active`, true when this stop follows a block.
    Stopping {
        #[serde(rename = "stopHookActive")]
        stop_hook_active: bool,
    },
    /// Claude's `SessionStart` hook after compaction. Answered with the read-back.
    Compacted,
    Disconnected {
        generation: String,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Hook {
    Delivery(DeliveryHook),
    Turn(TurnHook),
    Child(ChildHook),
    Classic(ClassicHook),
}

impl Hook {
    /// The gate, the read-back and a companion's disconnect, which send nothing to the
    /// session and so are answered even while sends are off, as in validation. A disconnect
    /// is the heartbeat's end, and heartbeats are recorded then too.
    fn sends_nothing(&self) -> bool {
        matches!(
            self,
            Self::Classic(ClassicHook {
                classic: Classic::Stopping { .. }
                    | Classic::Compacted
                    | Classic::Disconnected { .. },
                ..
            })
        )
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Outcome {
    Deferred,
    Dropped,
    Uncertain,
    Started {
        origin: String,
        #[serde(rename = "entryId")]
        entry_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
    },
}

/// The Claude plugin's hook events. Replies `{data:null}`, except `stopping`, which gets the
/// gate's `GateReply`, and `compacted`, which gets the read-back's `ReadbackReply`. A session
/// that leads no review gets `{block:false}` and an empty text for those two.
pub(super) async fn hook(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let command: Hook = entry.auth.bridge_command(&headers, &bytes, 16_384)?;
    if !entry.sends_allowed && !command.sends_nothing() {
        return Err(AppError::Unavailable.into());
    }
    let command = match command {
        Hook::Delivery(delivery) => delivery,
        Hook::Classic(call) => {
            let event = match call.classic {
                Classic::Stopping { stop_hook_active } => {
                    return gate_hook(&entry, &call.context, stop_hook_active).await;
                }
                Classic::Compacted => return read_back_hook(&entry, &call.context).await,
                Classic::PermissionPending => ClaudeClassicEvent::PermissionPending,
                Classic::Disconnected { generation } => {
                    ClaudeClassicEvent::Disconnected { generation }
                }
                Classic::Display {
                    turn_id,
                    message_id,
                    index,
                    final_chunk,
                } => ClaudeClassicEvent::Display {
                    turn: parse(&turn_id)?,
                    message: parse(&message_id)?,
                    index,
                    final_chunk,
                },
            };
            let (lead, _) = verified_context(&entry, call.context).await?;
            entry.store.record_claude_classic(lead, event).await?;
            return answered(&entry, ());
        }
        Hook::Child(child) => {
            let (root, history) = verified_context(&entry, child.context).await?;
            let companion = entry.claude.as_ref().ok_or(AppError::Unavailable)?;
            let (status, phase) = child.status.facts();
            let proof = companion
                .prove_child(
                    history,
                    ClaudeChild {
                        id: parse(&child.child_id)?,
                        generation: child.generation,
                        sequence: child.sequence,
                        parent: child.parent_id.as_deref().map(parse).transpose()?,
                        cwd: AbsolutePath::from_canonical(child.child_cwd)
                            .map_err(|_| AppError::InvalidRequest)?,
                        activity: reviewq_app::hosts::HostActivity {
                            phase,
                            has_questions: false,
                        },
                        metadata: reviewq_app::hosts::NativeSessionMetadata {
                            name: child_label(child.name, 512),
                            status: Some(status.to_owned()),
                            started_at_ms: child.started_at_ms.filter(|time| *time > 0),
                            model: child_label(child.model, 256),
                            effort: child_label(child.effort, 64),
                        },
                    },
                )
                .await?;
            entry.store.record_claude_child(root, proof).await?;
            return answered(&entry, ());
        }
        Hook::Turn(turn) => {
            let (lead, _) = verified_context(&entry, turn.context).await?;
            entry
                .store
                .record_claude_turn(
                    lead,
                    ClaudeTurnResult {
                        turn: parse(&turn.turn_id)?,
                        generation: turn.generation,
                        normal: turn.normal,
                    },
                )
                .await?;
            return answered(&entry, ());
        }
    };
    let (lead, session) = verified_context(&entry, command.context).await?;
    let offer = command.offer.checked()?;
    let replay = match &command.outcome {
        Outcome::Deferred => Some(ClaudeDeliveryAck::Deferred),
        Outcome::Dropped => Some(ClaudeDeliveryAck::Dropped),
        Outcome::Uncertain => None,
        Outcome::Started {
            origin,
            entry_id,
            turn_id,
        } if origin == "reviewq" => Some(ClaudeDeliveryAck::Started(ObservedInput {
            entry_id: parse(entry_id)?,
            turn_id: parse(turn_id)?,
        })),
        Outcome::Started { .. } => return Err(AppError::Forbidden.into()),
    };
    if let Some(replay) = replay
        && entry
            .store
            .replay_claude_delivery(lead.clone(), offer.clone(), replay)
            .await?
    {
        return answered(&entry, ());
    }
    let result = match command.outcome {
        Outcome::Deferred => ClaudeDeliveryResult::Deferred,
        Outcome::Dropped => ClaudeDeliveryResult::Dropped,
        Outcome::Uncertain => ClaudeDeliveryResult::Uncertain,
        Outcome::Started {
            origin,
            entry_id,
            turn_id,
        } => {
            if origin != "reviewq" {
                return Err(AppError::Forbidden.into());
            }
            let input = ObservedInput {
                entry_id: parse(&entry_id)?,
                turn_id: parse(&turn_id)?,
            };
            let pending = entry
                .store
                .claude_delivery(lead.clone(), offer.clone())
                .await?;
            let anchor = pending.anchor.ok_or(AppError::Conflict)?;
            let DeliveryWork::Dispatch { text, .. } = pending.claim.work else {
                return Err(AppError::Conflict.into());
            };
            let companion = entry.claude.as_ref().ok_or(AppError::Unavailable)?;
            companion
                .confirm_prompt(session, anchor.clone(), input.entry_id.clone(), text)
                .await?;
            ClaudeDeliveryResult::Started { anchor, input }
        }
    };
    entry
        .store
        .finish_claude_delivery(lead, offer, result)
        .await?;
    answered(&entry, ())
}

/// `stopping`: the gate's reply for the session's lead, or a release when the session leads
/// no review. The hook names no turn, so the gate counts its blocks per stop sequence.
async fn gate_hook(
    entry: &Entry,
    context: &ClaudeCommand,
    stop_hook_active: bool,
) -> Result<Response, ApiError> {
    let reply = match ledger_session(entry, context).await? {
        Some(session) => {
            let request = GateRequest {
                session,
                turn: None,
                stop_hook_active,
            };
            answer_gate(entry, request).await?
        }
        None => GateReply::allow(),
    };
    answered(entry, reply)
}

/// `compacted`: the read-back for the session's lead, or an empty text when the session
/// leads no review.
async fn read_back_hook(entry: &Entry, context: &ClaudeCommand) -> Result<Response, ApiError> {
    let reply = read_back(entry, ledger_session(entry, context).await?).await?;
    answered(entry, reply)
}

/// The linked session a gate or read-back hook names, from the store alone: neither reads the
/// transcript. `None` when the session leads no review.
async fn ledger_session(
    entry: &Entry,
    context: &ClaudeCommand,
) -> Result<Option<HookSession>, AppError> {
    match registered_lead(entry, context).await {
        Ok((lead, ..)) => Ok(Some(HookSession::Claude(lead.session_key().clone()))),
        Err(AppError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The hook reply: `data` in the common envelope.
fn answered<T: Serialize>(entry: &Entry, data: T) -> Result<Response, ApiError> {
    Ok(Json(success(data, *entry.commits.borrow())?).into_response())
}
