use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::hosts::ClaudeWorkflowStore;
use reviewq_app::workflow::{
    NativeCall, NativeWorkflowProof, WorkflowHold, WorkflowReadiness, WorkflowTool,
};
use reviewq_domain::identity::NativeTurnId;
use reviewq_domain::session::LeadTarget;

use super::Claude;
use crate::history::claude_workflow_opening;

pub struct ClaudeWorkflow {
    native: Arc<Claude>,
    state: Arc<dyn ClaudeWorkflowStore>,
}

impl ClaudeWorkflow {
    pub fn new(native: Arc<Claude>, state: Arc<dyn ClaudeWorkflowStore>) -> Self {
        Self { native, state }
    }
}

impl NativeWorkflowProof for ClaudeWorkflow {
    fn dispatch_readiness(&self, lead: LeadTarget) -> AppFuture<'_, WorkflowReadiness> {
        Box::pin(async move {
            let context = self.state.claude_workflow_context(lead, None).await?;
            if let Some(ready) = context.ready {
                return Ok(WorkflowReadiness::Ready(ready));
            }
            Ok(WorkflowReadiness::Held(if context.active_turn.is_some() {
                WorkflowHold::ActiveWork
            } else {
                WorkflowHold::UnknownSettings
            }))
        })
    }

    fn ended_and_idle(&self, lead: LeadTarget, turn: NativeTurnId) -> AppFuture<'_, bool> {
        Box::pin(async move {
            let current = self.state.claude_workflow_context(lead, Some(turn)).await?;
            Ok(current.idle && current.terminal.is_some())
        })
    }

    fn verify_active_call(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
        call_id: String,
        tool: WorkflowTool,
    ) -> AppFuture<'_, NativeCall> {
        self.verify_opening(lead, turn, call_id, tool)
    }
}

impl ClaudeWorkflow {
    fn verify_opening(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
        call_id: String,
        tool: WorkflowTool,
    ) -> AppFuture<'_, NativeCall> {
        Box::pin(async move {
            let context = self
                .state
                .claude_workflow_context(lead.clone(), None)
                .await?;
            let turn_id = context
                .active_turn
                .filter(|id| turn.as_ref().is_none_or(|turn| turn == id))
                .ok_or(AppError::NotFound)?;
            let id = call_id.clone();
            let opening = self
                .native
                .read_native(context.session, move |session, cancelled| {
                    claude_workflow_opening(&session, &id, tool, cancelled)
                })
                .await?;
            if context.request.is_none() || context.request != opening.request {
                return Err(AppError::Forbidden);
            }
            Ok(NativeCall {
                lead,
                turn_id,
                root_turn_id: None,
                call_id,
                tool,
                source_generation: opening.generation,
                source_position: opening.position,
                request_entry_id: opening.request,
            })
        })
    }
}
