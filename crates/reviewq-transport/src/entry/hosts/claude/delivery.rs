use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::hosts::{
    ClaudeChild, ClaudeClassicEvent, ClaudeDeliveryAck, ClaudeDeliveryResult, ClaudeTurnResult,
    DeliveryWork, ObservedInput,
};
use reviewq_domain::session::AbsolutePath;
use reviewq_protocol::Success;
use serde::{Deserialize, Serialize};

use super::{ClaudeCommand, ClaudeOffer, bridge_command, verified_context};
use crate::entry::error::ApiBytes;
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
}

pub(super) async fn check(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<Effect>>, ApiError> {
    let command: Check = bridge_command(&entry, &headers, &bytes, 16_384)?;
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
        text, report_ref, ..
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
    response: Option<FinalResponse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinalResponse {
    entry_id: String,
    sha256: String,
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
    working: bool,
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
    Stopping,
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

pub(super) async fn hook(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<()>>, ApiError> {
    let command: Hook = bridge_command(&entry, &headers, &bytes, 16_384)?;
    if !entry.sends_allowed {
        return Err(AppError::Unavailable.into());
    }
    let command = match command {
        Hook::Delivery(delivery) => delivery,
        Hook::Classic(call) => {
            let (lead, _) = verified_context(&entry, call.context).await?;
            let event = match call.classic {
                Classic::PermissionPending => ClaudeClassicEvent::PermissionPending,
                Classic::Stopping => ClaudeClassicEvent::Stopping,
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
            entry.store.record_claude_classic(lead, event).await?;
            return Ok(Json(success((), *entry.commits.borrow())?));
        }
        Hook::Child(child) => {
            let (root, history) = verified_context(&entry, child.context).await?;
            let companion = entry.claude.as_ref().ok_or(AppError::Unavailable)?;
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
                        working: child.working,
                    },
                )
                .await?;
            entry.store.record_claude_child(root, proof).await?;
            return Ok(Json(success((), *entry.commits.borrow())?));
        }
        Hook::Turn(turn) => {
            let (lead, _) = verified_context(&entry, turn.context).await?;
            let response = turn
                .response
                .map(|value| Ok::<_, AppError>((parse(&value.entry_id)?, parse(&value.sha256)?)))
                .transpose()?;
            entry
                .store
                .record_claude_turn(
                    lead,
                    ClaudeTurnResult {
                        turn: parse(&turn.turn_id)?,
                        generation: turn.generation,
                        normal: turn.normal,
                        response,
                    },
                )
                .await?;
            return Ok(Json(success((), *entry.commits.borrow())?));
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
        return Ok(Json(success((), *entry.commits.borrow())?));
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
    Ok(Json(success((), *entry.commits.borrow())?))
}
