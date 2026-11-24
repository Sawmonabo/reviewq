//! Selects the proof reader by the registered native binding.

use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::workflow::{
    ExpectedAcknowledgement, NativeCall, NativeWorkflowProof, ReportCompletion, WorkflowHold,
    WorkflowReadiness, WorkflowTool,
};
use reviewq_domain::identity::NativeTurnId;
use reviewq_domain::session::{HostProvider, LeadTarget};

use crate::Codex;

pub struct Workflows {
    codex: Option<Arc<Codex>>,
    claude: Option<Arc<dyn NativeWorkflowProof>>,
}

impl Workflows {
    pub fn new(codex: Option<Arc<Codex>>) -> Self {
        Self {
            codex,
            claude: None,
        }
    }

    pub fn with_claude(mut self, claude: Option<Arc<dyn NativeWorkflowProof>>) -> Self {
        self.claude = claude;
        self
    }

    fn reader(&self, lead: &LeadTarget) -> Result<&dyn NativeWorkflowProof, AppError> {
        match lead.binding().identity().provider() {
            HostProvider::Codex => self
                .codex
                .as_deref()
                .map(|codex| codex as &dyn NativeWorkflowProof),
            HostProvider::Claude => self.claude.as_deref(),
        }
        .ok_or(AppError::Unavailable)
    }

    fn readiness(&self, lead: LeadTarget, followup: bool) -> AppFuture<'_, WorkflowReadiness> {
        Box::pin(async move {
            match self.reader(&lead) {
                Ok(reader) if followup => reader.followup_readiness(lead).await,
                Ok(reader) => reader.dispatch_readiness(lead).await,
                Err(_) if lead.binding().identity().provider() == HostProvider::Claude => {
                    Ok(WorkflowReadiness::Held(WorkflowHold::MissingCompanion))
                }
                Err(_) => Ok(WorkflowReadiness::Held(WorkflowHold::ProviderUnavailable)),
            }
        })
    }
}

impl NativeWorkflowProof for Workflows {
    fn prepare_recovery(
        &self,
        candidate: &reviewq_app::hosts::recovery::Candidate,
    ) -> AppFuture<
        '_,
        (
            reviewq_app::hosts::recovery::RecoveryAuthorization,
            reviewq_app::workflow::ReadyWorkflow,
        ),
    > {
        let lead = candidate.lead.clone();
        let id = candidate.episode.id.clone();
        let turn = candidate.episode.turn.clone();
        let kind = candidate.episode.kind;
        let account = candidate.account.clone();
        let provider = candidate.model_provider.clone();
        Box::pin(async move {
            self.codex
                .as_ref()
                .ok_or(AppError::Unavailable)?
                .prepare_continuation(&lead, id, turn, kind, (account, provider), false)
                .await
        })
    }

    fn observe_attention(
        &self,
        target: reviewq_app::notifications::TrackedNative,
    ) -> AppFuture<'_, Option<reviewq_app::notifications::NativeAttention>> {
        Box::pin(async move {
            match target.session.binding.identity().provider() {
                HostProvider::Codex => {
                    self.codex
                        .as_ref()
                        .ok_or(AppError::Unavailable)?
                        .attention(target)
                        .await
                }
                HostProvider::Claude => Ok(None),
            }
        })
    }

    fn dispatch_readiness(&self, lead: LeadTarget) -> AppFuture<'_, WorkflowReadiness> {
        self.readiness(lead, false)
    }

    fn followup_readiness(&self, lead: LeadTarget) -> AppFuture<'_, WorkflowReadiness> {
        self.readiness(lead, true)
    }

    fn ended_and_idle(&self, lead: LeadTarget, turn: NativeTurnId) -> AppFuture<'_, bool> {
        Box::pin(async move { self.reader(&lead)?.ended_and_idle(lead, turn).await })
    }

    fn verify_active_call(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
        call_id: String,
        tool: WorkflowTool,
    ) -> AppFuture<'_, NativeCall> {
        Box::pin(async move {
            self.reader(&lead)?
                .verify_active_call(lead, turn, call_id, tool)
                .await
        })
    }

    fn verify_mcp_report_call(
        &self,
        lead: LeadTarget,
        call_id: String,
        arguments: Vec<u8>,
        origin_item_id: Option<String>,
    ) -> AppFuture<'_, NativeCall> {
        Box::pin(async move {
            self.reader(&lead)?
                .verify_mcp_report_call(lead, call_id, arguments, origin_item_id)
                .await
        })
    }

    fn verify_witnessed_report_call(
        &self,
        lead: LeadTarget,
        turn: NativeTurnId,
        call_id: String,
        arguments: Vec<u8>,
        ordinal: u32,
        origin_item_id: Option<String>,
    ) -> AppFuture<'_, NativeCall> {
        Box::pin(async move {
            self.reader(&lead)?
                .verify_witnessed_report_call(
                    lead,
                    turn,
                    call_id,
                    arguments,
                    ordinal,
                    origin_item_id,
                )
                .await
        })
    }

    fn report_completion(
        &self,
        call: NativeCall,
        acknowledgement: ExpectedAcknowledgement,
        include_response: bool,
    ) -> AppFuture<'_, ReportCompletion> {
        Box::pin(async move {
            self.reader(&call.lead)?
                .report_completion(call, acknowledgement, include_response)
                .await
        })
    }
}
