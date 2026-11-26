//! Lifecycle observations from the exact registered Claude companion.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use reviewq_domain::identity::{
    DeliveryFence, DeliveryId, NativeInputId, NativeSessionId, NativeTurnId, ReportHash, SessionKey,
};
use reviewq_domain::session::{AbsolutePath, LeadTarget, NativeBinding, SessionTarget};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::history::{FilePosition, HistorySession};
use crate::workflow::ReadyWorkflow;

use super::{ClaudeContext, DeliveryClaim, HostActivity, ObservedInput};

mod process;
pub use process::ClaudeHost;

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
    /// The process hosting the session, as the heartbeat helper named its parent.
    pub host: Option<ClaudeHost>,
    /// The SHA-256 of the companion proof the report carried, which its generation's first
    /// report registers and every later report and plugin request must match.
    pub proof: [u8; 32],
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
            || (self.latest_prompt_manual && self.latest_prompt_turn.is_none())
            || self
                .model
                .as_ref()
                .is_some_and(|value| !setting(value, 128))
            || self
                .effort
                .as_ref()
                .is_some_and(|value| !setting(value, 64))
            || self.host.as_ref().is_some_and(|host| !host.is_valid())
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

/// Exact canonical native result, independent of JSON field order.
pub fn claude_answer_digest(answers: &ClaudeQuestionAnswers) -> Option<ReportHash> {
    if !(1..=4).contains(&answers.len()) {
        return None;
    }
    let mut digest = Sha256::new();
    for (prompt, answer) in answers {
        if prompt.is_empty() || prompt.len() > 8192 || answer.is_empty() || answer.len() > 8192 {
            return None;
        }
        for field in [prompt, answer] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
    }
    Some(ReportHash::from_sha256(digest.finalize().into()))
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

#[derive(Clone)]
pub struct ClaudeTurnResult {
    pub turn: NativeTurnId,
    pub generation: String,
    pub normal: bool,
}

pub struct ClaudeWorkflowContext {
    pub session: HistorySession,
    pub active_turn: Option<NativeTurnId>,
    pub active_prompt: Option<NativeInputId>,
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
    /// Claude began or finished displaying a message, which the session's history shows.
    Display,
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

    /// Admits a request of the Claude plugin for `session` when `proof`, the SHA-256 of the
    /// proof it carried, is the one the session's companion registered. A session ReviewQ does
    /// not store is admitted, so the call gets its own answer for that. With `admit_gone`, so is
    /// a session whose companion is gone or was never stored. Fails with `Unauthorized`
    /// otherwise, a stored session with no companion included.
    fn prove_claude_companion(
        &self,
        session: SessionKey,
        proof: Option<[u8; 32]>,
        admit_gone: bool,
    ) -> AppFuture<'_, ()>;
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
