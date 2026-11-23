//! Lifecycle observations from the exact registered Claude companion.

use std::collections::BTreeMap;

use reviewq_domain::identity::{
    DeliveryFence, DeliveryId, NativeInputId, NativeSessionId, NativeTurnId, ReportHash,
};
use reviewq_domain::session::{AbsolutePath, LeadTarget, NativeBinding, SessionTarget};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::history::{FilePosition, HistorySession};
use crate::workflow::ReadyWorkflow;

use super::{ClaudeContext, DeliveryClaim, HostActivity, ObservedInput};

pub struct ClaudeHeartbeat {
    pub generation: String,
    pub sequence: u64,
    pub activity: Option<HostActivity>,
    pub turn: Option<NativeTurnId>,
    pub step: Option<u64>,
    pub question_id: Option<NativeInputId>,
    pub latest_prompt: Option<NativeInputId>,
    pub latest_prompt_turn: Option<NativeTurnId>,
    pub latest_prompt_manual: bool,
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

#[derive(Clone, Eq, PartialEq)]
pub struct ClaudeQuestionAddress {
    pub generation: String,
    pub turn: NativeTurnId,
    pub assistant: NativeInputId,
    pub call: NativeInputId,
}

pub type ClaudeQuestionAnswers = BTreeMap<String, String>;

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

#[derive(Clone)]
pub struct ClaudeTurnResult {
    pub turn: NativeTurnId,
    pub generation: String,
    pub normal: bool,
    pub response: Option<(NativeInputId, ReportHash)>,
}

pub struct ClaudeWorkflowContext {
    pub session: HistorySession,
    pub active_turn: Option<NativeTurnId>,
    pub idle: bool,
    pub request: Option<NativeInputId>,
    pub ready: Option<ReadyWorkflow>,
    pub terminal: Option<ClaudeTurnResult>,
}

pub trait ClaudeWorkflowStore: Send + Sync {
    fn claude_workflow_context(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
    ) -> AppFuture<'_, ClaudeWorkflowContext>;

    fn record_claude_turn(&self, lead: LeadTarget, result: ClaudeTurnResult) -> AppFuture<'_, ()>;
}

pub struct ClaudeChild {
    pub generation: String,
    pub sequence: u64,
    pub id: NativeSessionId,
    pub parent: Option<NativeSessionId>,
    pub cwd: AbsolutePath,
    pub activity: super::HostActivity,
    pub metadata: super::NativeSessionMetadata,
}

pub struct ProvenClaudeChild {
    pub generation: String,
    pub sequence: u64,
    pub binding: NativeBinding,
    pub parent: Option<NativeSessionId>,
    pub source: AbsolutePath,
    pub activity: super::HostActivity,
    pub metadata: super::NativeSessionMetadata,
}

pub enum ClaudeClassicEvent {
    PermissionPending,
    Display {
        turn: NativeTurnId,
        message: NativeInputId,
        index: u64,
        final_chunk: bool,
    },
    Stopping,
    Disconnected {
        generation: String,
    },
}

pub trait ClaudeStore: Send + Sync {
    fn fence_claude_question(
        &self,
        lead: LeadTarget,
        address: ClaudeQuestionAddress,
        invalidate: bool,
    ) -> AppFuture<'_, ()>;

    fn claude_companion_available(
        &self,
        actor: Actor,
        target: SessionTarget,
    ) -> AppFuture<'_, Option<bool>>;

    fn pending_claude_launch(&self, context: ClaudeContext) -> AppFuture<'_, bool>;

    fn record_claude_classic(
        &self,
        lead: LeadTarget,
        event: ClaudeClassicEvent,
    ) -> AppFuture<'_, ()>;

    fn record_claude_child(&self, lead: LeadTarget, child: ProvenClaudeChild) -> AppFuture<'_, ()>;

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

    fn replay_claude_delivery(
        &self,
        lead: LeadTarget,
        offer: ClaudeOffer,
        outcome: ClaudeDeliveryAck,
    ) -> AppFuture<'_, bool>;

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

pub enum ClaudeDeliveryAck {
    Deferred,
    Dropped,
    Started(ObservedInput),
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
