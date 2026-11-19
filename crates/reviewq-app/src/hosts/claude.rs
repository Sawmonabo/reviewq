//! Lifecycle observations from the exact registered Claude companion.

use reviewq_domain::identity::{DeliveryFence, DeliveryId, NativeInputId, NativeTurnId};
use reviewq_domain::session::LeadTarget;

use crate::access::{AppError, AppFuture};
use crate::events::Snapshot;
use crate::history::FilePosition;
use crate::workflow::ReadyWorkflow;

use super::{DeliveryClaim, HostActivity, ObservedInput};

pub struct ClaudeHeartbeat {
    pub generation: String,
    pub sequence: u64,
    pub activity: Option<HostActivity>,
    pub turn: Option<NativeTurnId>,
    pub latest_prompt: Option<NativeInputId>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl ClaudeHeartbeat {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.generation.len() != 32
            || !self
                .generation
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || self.sequence == 0
            || self.sequence > 9_007_199_254_740_991
            || self
                .model
                .as_ref()
                .is_some_and(|value| !setting(value, 128))
            || self
                .effort
                .as_ref()
                .is_some_and(|value| !setting(value, 64))
        {
            return Err(AppError::InvalidRequest);
        }
        Ok(())
    }
}

pub struct ClaudeHeartbeatResult {
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeOffer {
    pub id: DeliveryId,
    pub fence: DeliveryFence,
    pub generation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeAnchor {
    pub source_generation: String,
    pub file: FilePosition,
}

#[derive(Clone, Debug)]
pub struct ClaudeSend {
    pub claim: DeliveryClaim,
    pub anchor: Option<ClaudeAnchor>,
    pub ready: Option<ReadyWorkflow>,
}

pub trait ClaudeStore: Send + Sync {
    fn offer_claude_delivery(
        &self,
        lead: LeadTarget,
        generation: String,
    ) -> AppFuture<'_, Option<ClaudeOffer>>;

    fn claude_delivery(&self, lead: LeadTarget, offer: ClaudeOffer) -> AppFuture<'_, ClaudeSend>;

    fn authorize_claude_delivery(
        &self,
        lead: LeadTarget,
        offer: ClaudeOffer,
        anchor: ClaudeAnchor,
        checked_workflow: Option<ReadyWorkflow>,
    ) -> AppFuture<'_, DeliveryClaim>;

    fn finish_claude_delivery(
        &self,
        lead: LeadTarget,
        offer: ClaudeOffer,
        result: ClaudeDeliveryResult,
    ) -> AppFuture<'_, ()>;

    fn record_claude_heartbeat(
        &self,
        lead: LeadTarget,
        observation: ClaudeHeartbeat,
    ) -> AppFuture<'_, Snapshot<ClaudeHeartbeatResult>>;
}

pub enum ClaudeDeliveryResult {
    Deferred,
    Dropped,
    Started {
        anchor: ClaudeAnchor,
        input: ObservedInput,
    },
    Uncertain,
}

fn setting(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}
