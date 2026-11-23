use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::hosts::ClaudeWorkflowStore;
use reviewq_app::workflow::{
    CompletedReportCall, ExpectedAcknowledgement, NativeCall, NativeWorkflowProof,
    ReportCompletion, ReportContent, WorkflowHold, WorkflowReadiness, WorkflowTool,
};
use reviewq_domain::identity::NativeTurnId;
use reviewq_domain::session::LeadTarget;

use super::Claude;
use crate::history::{
    claude_workflow_opening, claude_workflow_prompt, claude_workflow_result, native_arguments_hash,
};

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
        self.verify_opening(lead, turn, call_id, tool, None)
    }

    fn verify_mcp_report_call(
        &self,
        lead: LeadTarget,
        call_id: String,
        arguments: Vec<u8>,
        _origin_item_id: Option<String>,
    ) -> AppFuture<'_, NativeCall> {
        self.verify_opening(lead, None, call_id, WorkflowTool::Report, Some(arguments))
    }

    fn verify_witnessed_report_call(
        &self,
        lead: LeadTarget,
        turn: NativeTurnId,
        call_id: String,
        arguments: Vec<u8>,
        ordinal: u32,
        _origin_item_id: Option<String>,
    ) -> AppFuture<'_, NativeCall> {
        Box::pin(async move {
            if ordinal == 0 {
                return Err(AppError::InvalidRequest);
            }
            let context = self
                .state
                .claude_workflow_context(lead.clone(), None)
                .await?;
            if context.active_turn.as_ref() != Some(&turn) {
                return Err(AppError::Conflict);
            }
            let prompt = context.active_prompt.ok_or(AppError::NotFound)?;
            let arguments =
                serde_json::from_slice(&arguments).map_err(|_| AppError::InvalidRequest)?;
            let hash = native_arguments_hash(arguments)?;
            let (source_generation, source_position) = self
                .native
                .read_native(context.session, {
                    let prompt = prompt.clone();
                    move |session, cancelled| claude_workflow_prompt(&session, &prompt, cancelled)
                })
                .await?;
            Ok(NativeCall {
                lead,
                turn_id: turn,
                root_turn_id: None,
                call_id,
                source_generation,
                source_position,
                source_ordinal: ordinal,
                arguments_sha256: Some(hash),
                request_entry_id: Some(prompt),
                tool: WorkflowTool::Report,
            })
        })
    }

    fn report_completion(
        &self,
        call: NativeCall,
        acknowledgement: ExpectedAcknowledgement,
        include_response: bool,
    ) -> AppFuture<'_, ReportCompletion> {
        Box::pin(async move {
            if call.tool != WorkflowTool::Report {
                return Err(AppError::Forbidden);
            }
            let context = self
                .state
                .claude_workflow_context(call.lead.clone(), Some(call.turn_id.clone()))
                .await?;
            let normal = context
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.normal);
            let response = context
                .terminal
                .as_ref()
                .filter(|_| include_response)
                .and_then(|terminal| terminal.response.clone());
            let candidate = call.clone();
            let ack = acknowledgement.clone();
            let result = self
                .native
                .read_native(context.session, move |session, cancelled| {
                    claude_workflow_result(&session, &candidate, &ack, response, cancelled)
                })
                .await;
            let found = match result {
                Ok(Some(result)) => result,
                Ok(None)
                    if context.terminal.is_none()
                        || (call.arguments_sha256.is_some() && context.terminal_settling) =>
                {
                    return Ok(ReportCompletion::Pending);
                }
                Ok(None) | Err(AppError::Conflict) => return Ok(ReportCompletion::Unproved),
                Err(error) => return Err(error),
            };
            if context.terminal.is_none() && context.active_turn.as_ref() == Some(&call.turn_id) {
                return Ok(ReportCompletion::Pending);
            }
            if include_response && normal && found.response.is_none() && context.terminal_settling {
                return Ok(ReportCompletion::Pending);
            }
            let final_response = if normal {
                found.response.map(|text| {
                    Arc::new(VerifiedResponse(text.into_boxed_slice())) as Arc<dyn ReportContent>
                })
            } else {
                None
            };
            Ok(ReportCompletion::Confirmed(Box::new(CompletedReportCall {
                call,
                acknowledged: acknowledgement,
                result_entry_id: found.entry,
                normal_turn_completion: normal,
                final_response,
            })))
        })
    }
}

impl ClaudeWorkflow {
    fn verify_opening(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
        call_id: String,
        tool: WorkflowTool,
        arguments: Option<Vec<u8>>,
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
            let arguments = arguments
                .as_deref()
                .map(serde_json::from_slice::<serde_json::Value>)
                .transpose()
                .map_err(|_| AppError::InvalidRequest)?;
            let arguments_sha256 = arguments.clone().map(native_arguments_hash).transpose()?;
            let id = call_id.clone();
            let opening = self
                .native
                .read_native(context.session, move |session, cancelled| {
                    claude_workflow_opening(&session, &id, tool, arguments.as_ref(), cancelled)
                })
                .await?;
            if tool == WorkflowTool::Action
                && (context.request.is_none() || context.request != opening.request)
            {
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
                arguments_sha256,
                source_ordinal: opening.ordinal,
                request_entry_id: opening.request,
            })
        })
    }
}

struct VerifiedResponse(Box<[u8]>);

impl ReportContent for VerifiedResponse {
    fn byte_length(&self) -> u64 {
        self.0.len() as u64
    }

    fn read(&self, offset: u64, length: usize) -> AppFuture<'_, Vec<u8>> {
        Box::pin(async move {
            let start: usize = offset
                .try_into()
                .map_err(|_| AppError::RangeNotSatisfiable)?;
            let end = start
                .checked_add(length)
                .ok_or(AppError::RangeNotSatisfiable)?;
            self.0
                .get(start..end)
                .map(<[u8]>::to_vec)
                .ok_or(AppError::RangeNotSatisfiable)
        })
    }

    fn verify_unchanged(&self) -> AppFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
