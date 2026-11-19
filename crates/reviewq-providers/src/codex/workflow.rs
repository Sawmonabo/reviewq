use std::path::Path;
use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::workflow::{
    CompletedReportCall, ExpectedAcknowledgement, NativeCall, NativeWorkflowProof,
    ReportCompletion, ReportContent, WorkflowTool,
};
use reviewq_domain::identity::{NativeInputId, NativeTurnId};
use reviewq_domain::session::{HostProvider, LeadTarget};
use serde::Deserialize;

use super::Codex;
use super::history::{Item, Turn};
use super::session::{Thread, ThreadStatus};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Acknowledgement {
    receipt_id: String,
    declaration_sha256: String,
    state: String,
}

impl Acknowledgement {
    pub(crate) fn matches(&self, expected: &ExpectedAcknowledgement) -> bool {
        matches!(self.state.as_str(), "pending_completion" | "recorded")
            && self.receipt_id == expected.receipt_id.as_str()
            && self.declaration_sha256 == expected.declaration_sha256.as_str()
    }
}

impl NativeWorkflowProof for Codex {
    fn verify_active_call(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
        call_id: String,
        tool: WorkflowTool,
    ) -> AppFuture<'_, NativeCall> {
        Box::pin(async move {
            let thread = self.verified_thread(&lead).await?;
            if !matches!(thread.status, ThreadStatus::Active { .. }) {
                return Err(AppError::NotFound);
            }
            let mut turns = self.turns(&thread.id, None, false, 1, true).await?;
            let turn = turns
                .data
                .pop()
                .filter(|native| {
                    native.status == "inProgress"
                        && turn.as_ref().is_none_or(|turn| native.id == turn.as_str())
                })
                .ok_or(AppError::NotFound)?;
            let index = call_position(&turn, &call_id, tool).ok_or(AppError::NotFound)?;
            if !matches!(&turn.items[index], Item::CommandExecution { status, .. } if status == "inProgress")
            {
                return Err(AppError::NotFound);
            }
            if turn.items[index + 1..]
                .iter()
                .any(|item| matches!(item, Item::UserMessage { .. }))
            {
                return Err(AppError::Conflict);
            }
            let (source_generation, source_position) =
                workflow_source(&lead, &thread, &call_id, None).await?;
            Ok(NativeCall {
                lead,
                turn_id: turn.id.parse().map_err(|_| AppError::Unavailable)?,
                call_id,
                source_generation,
                source_position,
                request_entry_id: request_address(&turn, index)?,
                tool,
            })
        })
    }

    fn report_completion(
        &self,
        call: NativeCall,
        acknowledgement: ExpectedAcknowledgement,
    ) -> AppFuture<'_, ReportCompletion> {
        Box::pin(async move {
            if call.tool != WorkflowTool::Report {
                return Err(AppError::Forbidden);
            }
            let native = call.lead.binding().identity();
            if native.provider() != HostProvider::Codex || native.profile() != &self.profile {
                return Err(AppError::Unavailable);
            }
            let thread = self.read_thread(native.native_id()).await?;
            let (generation, _) = workflow_source(
                &call.lead,
                &thread,
                &call.call_id,
                Some(call.source_position),
            )
            .await?;
            if generation != call.source_generation {
                return Ok(ReportCompletion::Unproved);
            }
            let Some(turn) = self.workflow_turn(&thread, &call.turn_id).await? else {
                return Ok(ReportCompletion::Pending);
            };
            let Some(index) = call_position(&turn, &call.call_id, call.tool) else {
                return Ok(ReportCompletion::Unproved);
            };
            if request_address(&turn, index)? != call.request_entry_id {
                return Ok(ReportCompletion::Unproved);
            }
            let Item::CommandExecution {
                id,
                status,
                exit_code,
                aggregated_output,
                ..
            } = &turn.items[index]
            else {
                return Ok(ReportCompletion::Unproved);
            };
            if status == "inProgress" || turn.status == "inProgress" {
                return Ok(ReportCompletion::Pending);
            }
            if status != "completed"
                || *exit_code != Some(0)
                || !aggregated_output
                    .as_deref()
                    .filter(|value| value.len() <= 8192)
                    .and_then(|value| serde_json::from_str::<Acknowledgement>(value).ok())
                    .is_some_and(|ack| ack.matches(&acknowledgement))
            {
                return Ok(ReportCompletion::Unproved);
            }
            let normal_turn_completion = turn.status == "completed";
            let final_response = if normal_turn_completion {
                turn.items[index + 1..]
                    .iter()
                    .rev()
                    .find_map(|item| match item {
                        Item::AgentMessage {
                            text,
                            phase: Some(phase),
                            ..
                        } if phase == "final_answer" && !text.is_empty() => {
                            Some(Arc::new(CapturedResponse(text.as_bytes().into()))
                                as Arc<dyn ReportContent>)
                        }
                        _ => None,
                    })
            } else {
                None
            };
            Ok(ReportCompletion::Confirmed(CompletedReportCall {
                call,
                acknowledged: acknowledgement,
                result_entry_id: id.parse().map_err(|_| AppError::Unavailable)?,
                normal_turn_completion,
                final_response,
            }))
        })
    }
}

impl Codex {
    async fn workflow_turn(
        &self,
        thread: &Thread,
        id: &NativeTurnId,
    ) -> Result<Option<Turn>, AppError> {
        let mut cursor = None;
        for _ in 0..8 {
            let mut page = self
                .turns(&thread.id, cursor.as_deref(), false, 1, true)
                .await?;
            if let Some(turn) = page.data.pop().filter(|turn| turn.id == id.as_str()) {
                return Ok(Some(turn));
            }
            let Some(next) = page.next_cursor.take() else {
                break;
            };
            if cursor.as_ref() == Some(&next) {
                break;
            }
            cursor = Some(next);
        }
        Ok(None)
    }
}

async fn workflow_source(
    lead: &LeadTarget,
    thread: &Thread,
    call_id: &str,
    position: Option<u64>,
) -> Result<(String, u64), AppError> {
    let path = thread.path.as_ref().ok_or(AppError::Unavailable)?;
    crate::history::codex_opening(
        lead.binding().clone(),
        path.clone(),
        call_id.to_owned(),
        position,
    )
    .await
}

fn call_position(turn: &Turn, call_id: &str, expected: WorkflowTool) -> Option<usize> {
    turn.items.iter().position(|item| matches!(item,
        Item::CommandExecution { id, command, source, .. }
            if id == call_id && matches!(source.as_deref(), Some("agent" | "unifiedExecStartup")) && reviewq_command(command, expected)))
}

fn request_address(turn: &Turn, index: usize) -> Result<Option<NativeInputId>, AppError> {
    turn.items[..index]
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::UserMessage { id, .. } => Some(id.parse().map_err(|_| AppError::Unavailable)),
            _ => None,
        })
        .transpose()
}

fn reviewq_command(command: &str, tool: WorkflowTool) -> bool {
    if command.len() > 8192 {
        return false;
    }
    let Some(arguments) = shlex::split(command) else {
        return false;
    };
    let inner = match arguments.as_slice() {
        [shell, flag, script]
            if matches!(
                Path::new(shell).file_name().and_then(|name| name.to_str()),
                Some("sh" | "bash" | "zsh")
            ) && matches!(flag.as_str(), "-c" | "-lc") =>
        {
            script
        }
        _ => command,
    };
    if inner.contains(['\n', '\r', ';', '&', '|', '`', '$', '<', '>']) {
        return false;
    }
    let Some(arguments) = shlex::split(inner) else {
        return false;
    };
    let [executable, category, operation, ..] = arguments.as_slice() else {
        return false;
    };
    if Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("reviewq")
    {
        return false;
    }
    match tool {
        WorkflowTool::Report => category == "report" && operation == "complete",
        WorkflowTool::Action => category == "action" && operation == "declare",
    }
}

struct CapturedResponse(Arc<[u8]>);

impl ReportContent for CapturedResponse {
    fn byte_length(&self) -> u64 {
        self.0.len() as u64
    }

    fn read(&self, offset: u64, length: usize) -> AppFuture<'_, Vec<u8>> {
        Box::pin(async move {
            let start = usize::try_from(offset).map_err(|_| AppError::InvalidRequest)?;
            let end = start.saturating_add(length).min(self.0.len());
            self.0
                .get(start..end)
                .map(<[u8]>::to_vec)
                .ok_or(AppError::InvalidRequest)
        })
    }

    fn verify_unchanged(&self) -> AppFuture<'_, ()> {
        // This owned snapshot was captured in the same provider response as the proven completion.
        Box::pin(async { Ok(()) })
    }
}
