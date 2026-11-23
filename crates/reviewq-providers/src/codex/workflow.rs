use std::path::Path;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::workflow::{
    CompletedReportCall, ExpectedAcknowledgement, NativeCall, NativeWorkflowProof, ReadyWorkflow,
    ReportCompletion, ReportFailureAddress, WorkflowHold, WorkflowReadiness, WorkflowTool,
};
use reviewq_domain::identity::{NativeInputId, NativeTurnId};
use reviewq_domain::session::LeadTarget;
use serde::Deserialize;

use super::Codex;
use super::history::{Item, Turn};
use super::history_reads::ItemWindow;
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
    fn dispatch_readiness(&self, lead: LeadTarget) -> AppFuture<'_, WorkflowReadiness> {
        Box::pin(async move {
            let Ok(thread) = self.verified_thread(&lead).await else {
                return Ok(WorkflowReadiness::Held(WorkflowHold::ProviderUnavailable));
            };
            self.idle_readiness(&lead, &thread).await
        })
    }

    fn ended_and_idle(&self, lead: LeadTarget, turn: NativeTurnId) -> AppFuture<'_, bool> {
        Box::pin(async move {
            let thread = self.retained_thread(&lead).await?;
            if !matches!(thread.status, ThreadStatus::Idle) {
                return Ok(false);
            }
            let Some(exact) = self.original_turn(&thread, &turn).await? else {
                return Ok(false);
            };
            Ok(matches!(
                exact.status.as_str(),
                "completed" | "interrupted" | "failed"
            ) && matches!(
                self.retained_thread(&lead).await?.status,
                ThreadStatus::Idle
            ))
        })
    }

    fn verify_active_call(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
        call_id: String,
        tool: WorkflowTool,
    ) -> AppFuture<'_, NativeCall> {
        Box::pin(async move {
            let (thread, active) = self
                .active_workflow_turn(&lead, turn.as_ref(), true)
                .await?;
            let index = call_position(&active, &call_id, tool).ok_or(AppError::NotFound)?;
            if !matches!(&active.items[index], Item::CommandExecution { status, .. } if status == "inProgress")
            {
                return Err(AppError::NotFound);
            }
            let (generation, position) = workflow_source(&lead, &thread, &call_id, None).await?;
            if active.items[index + 1..]
                .iter()
                .any(|item| matches!(item, Item::UserMessage { .. }))
            {
                return Err(AppError::Conflict);
            }
            Ok(NativeCall {
                lead,
                turn_id: active.id.parse().map_err(|_| AppError::Unavailable)?,
                root_turn_id: root_turn(&active)?,
                call_id,
                source_generation: generation,
                source_position: position,
                source_ordinal: 0,
                request_entry_id: request_address(&active, index)?,
                tool,
            })
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
            let arguments: serde_json::Value =
                serde_json::from_slice(&arguments).map_err(|_| AppError::InvalidRequest)?;
            let (thread, turn) = self.active_workflow_turn(&lead, None, false).await?;
            let turn_id: NativeTurnId = turn.id.parse().map_err(|_| AppError::Unavailable)?;
            let (ordinal, request) = self
                .mcp_report_position(&thread.id, &turn_id, &call_id, &arguments)
                .await?;
            let (generation, position, nested) = crate::history::codex_mcp_opening(
                lead.binding().clone(),
                thread.path.ok_or(AppError::Unavailable)?,
                turn_id.clone(),
                call_id.clone(),
                origin_item_id,
            )
            .await?;
            Ok(NativeCall {
                lead,
                root_turn_id: root_turn(&turn)?,
                turn_id,
                call_id,
                tool: WorkflowTool::Report,
                source_generation: generation,
                source_position: position,
                source_ordinal: if nested { ordinal } else { 0 },
                request_entry_id: request,
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
            let thread = self.retained_thread(&call.lead).await?;
            let path = thread.path.ok_or(AppError::Unavailable)?;
            let result_entry: NativeInputId =
                call.call_id.parse().map_err(|_| AppError::Conflict)?;
            let result = self
                .exact_item(&thread.id, &call.turn_id, &result_entry)
                .await;
            let item = match result {
                Ok(item) => item.item,
                Err(AppError::NotFound) => {
                    let ended = crate::history::codex_report_terminal(call, path, false).await?;
                    return Ok(if ended.is_some() {
                        ReportCompletion::Unproved
                    } else {
                        ReportCompletion::Pending
                    });
                }
                Err(error) => return Err(error),
            };
            let mcp = matches!(&item, Item::McpToolCall { .. });
            let (status, acknowledged) = match item {
                Item::McpToolCall {
                    id,
                    server,
                    tool,
                    status,
                    result,
                    ..
                } if id == call.call_id && report_tool(&server, &tool) => {
                    (status, result.as_ref().and_then(mcp_acknowledgement))
                }
                Item::CommandExecution {
                    id,
                    status,
                    command,
                    source,
                    exit_code,
                    aggregated_output,
                } if id == call.call_id
                    && matches!(source.as_deref(), Some("agent" | "unifiedExecStartup"))
                    && reviewq_command(&command, call.tool) =>
                {
                    let acknowledgement = aggregated_output
                        .as_deref()
                        .filter(|_| exit_code == Some(0))
                        .and_then(read_acknowledgement);
                    (status, acknowledgement)
                }
                _ => return Ok(ReportCompletion::Unproved),
            };
            if status == "inProgress" {
                return Ok(ReportCompletion::Pending);
            }
            if status != "completed"
                || !acknowledged.is_some_and(|ack| ack.matches(&acknowledgement))
            {
                return Ok(ReportCompletion::Unproved);
            }
            let Some(evidence) =
                crate::history::codex_report_terminal(call.clone(), path, include_response).await?
            else {
                return Ok(ReportCompletion::Pending);
            };
            if mcp && !evidence.result_seen {
                return Ok(ReportCompletion::Unproved);
            }
            Ok(ReportCompletion::Confirmed(Box::new(CompletedReportCall {
                call,
                acknowledged: acknowledgement,
                result_entry_id: result_entry,
                normal_turn_completion: evidence.normal,
                final_response: evidence.response,
            })))
        })
    }

    fn earlier_report_failures(
        &self,
        call: NativeCall,
        candidates: Vec<ReportFailureAddress>,
    ) -> AppFuture<'_, Vec<ReportFailureAddress>> {
        Box::pin(async move {
            let thread = self.retained_thread(&call.lead).await?;
            let path = thread.path.ok_or(AppError::Unavailable)?;
            crate::history::codex_earlier_report_failures(call, path, candidates).await
        })
    }
}

impl Codex {
    async fn active_workflow_turn(
        &self,
        lead: &LeadTarget,
        turn: Option<&NativeTurnId>,
        full: bool,
    ) -> Result<(Thread, Turn), AppError> {
        let thread = self.verified_thread(lead).await?;
        if !matches!(thread.status, ThreadStatus::Active { .. }) {
            return Err(AppError::NotFound);
        }
        let active = self
            .turns(&thread.id, None, false, 1, full)
            .await?
            .data
            .pop()
            .filter(|native| {
                native.status == "inProgress" && turn.is_none_or(|turn| native.id == turn.as_str())
            })
            .ok_or(AppError::NotFound)?;
        Ok((thread, active))
    }

    /// The host sorts this page by immutable creation ordinal. Defer completed tool output
    /// so earlier large command logs cannot prevent verification of the current handoff.
    async fn mcp_report_position(
        &self,
        thread: &str,
        turn: &NativeTurnId,
        call: &str,
        arguments: &serde_json::Value,
    ) -> Result<(u32, Option<NativeInputId>), AppError> {
        let mut cursor: Option<String> = None;
        let mut limit = 64;
        let mut ordinal = 0u32;
        let mut request = None;
        let mut matched = None;
        loop {
            let window = ItemWindow {
                forward: true,
                cursor: cursor.as_deref(),
                anchor: None,
                limit,
                defer: true,
            };
            let page = match self.read_item_window(thread, turn, window).await {
                Err(AppError::Unavailable) if limit > 1 => {
                    limit /= 2;
                    continue;
                }
                result => result?,
            };
            for entry in page.data {
                ordinal = ordinal.checked_add(1).ok_or(AppError::Unavailable)?;
                match entry.item {
                    Item::UserMessage { id, .. } => {
                        if matched.is_some() {
                            return Err(AppError::Conflict);
                        }
                        request = Some(id.parse().map_err(|_| AppError::Unavailable)?);
                    }
                    Item::McpToolCall {
                        id,
                        server,
                        tool,
                        status,
                        arguments: native,
                        ..
                    } if id == call => {
                        if matched.is_some()
                            || !report_tool(&server, &tool)
                            || status != "inProgress"
                            || native.as_ref() != Some(arguments)
                        {
                            return Err(AppError::Conflict);
                        }
                        matched = Some(ordinal);
                    }
                    _ => {}
                }
            }
            let Some(next) = page.next_cursor else { break };
            if cursor.as_ref() == Some(&next) {
                return Err(AppError::Unavailable);
            }
            cursor = Some(next);
        }
        Ok((matched.ok_or(AppError::NotFound)?, request))
    }

    pub(super) async fn idle_readiness(
        &self,
        lead: &LeadTarget,
        thread: &Thread,
    ) -> Result<WorkflowReadiness, AppError> {
        self.workflow_readiness(lead, thread, None).await
    }
    pub(super) async fn workflow_readiness(
        &self,
        lead: &LeadTarget,
        thread: &Thread,
        failed: Option<&NativeTurnId>,
    ) -> Result<WorkflowReadiness, AppError> {
        use WorkflowReadiness::{Held, Ready};
        match &thread.status {
            ThreadStatus::Idle => {}
            ThreadStatus::Active { active_flags } if active_flags.is_empty() => {
                return Ok(Held(WorkflowHold::ActiveWork));
            }
            ThreadStatus::Active { .. } => return Ok(Held(WorkflowHold::PendingNativeInput)),
            _ => return Ok(Held(WorkflowHold::ProviderUnavailable)),
        }
        let mut page = self.turns(&thread.id, None, false, 1, true).await?;
        let Some(turn) = page.data.pop() else {
            return Ok(Held(WorkflowHold::UnknownSettings));
        };
        match failed {
            Some(id) if turn.id != id.as_str() || turn.status != "failed" => {
                return Err(AppError::StaleRevision);
            }
            None if turn.status != "completed" => return Ok(Held(WorkflowHold::ActiveWork)),
            _ => {}
        }
        let mut latest: Option<NativeInputId> = None;
        for item in turn.items.iter().rev() {
            match item {
                Item::AgentMessage {
                    questions: Some(questions),
                    ..
                } if !questions.is_empty() => return Ok(Held(WorkflowHold::PendingNativeInput)),
                Item::UserMessage { id, .. } => {
                    latest = Some(id.parse().map_err(|_| AppError::Unavailable)?);
                    break;
                }
                _ => {}
            }
        }
        let Some(path) = &thread.path else {
            return Ok(Held(WorkflowHold::UnknownSettings));
        };
        let binding = lead.binding().clone();
        let settings = if failed.is_some() {
            crate::history::codex_exact_turn_settings(binding, path.clone(), turn.id.clone()).await
        } else {
            crate::history::codex_turn_settings(binding, path.clone(), turn.id.clone()).await
        };
        let (model, effort) = match settings {
            Ok(Some(settings)) => settings,
            _ => return Ok(Held(WorkflowHold::UnknownSettings)),
        };
        if model.is_empty()
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            || !matches!(
                effort.as_str(),
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )
        {
            return Ok(Held(WorkflowHold::UnknownSettings));
        }
        Ok(Ready(ReadyWorkflow {
            model,
            effort,
            latest_user_turn_id: latest
                .as_ref()
                .map(|_| turn.id.parse().map_err(|_| AppError::Unavailable))
                .transpose()?,
            latest_user_entry_id: latest,
        }))
    }
}

fn report_tool(server: &str, tool: &str) -> bool {
    server == "reviewq" && tool == "report_handoff"
}

fn mcp_acknowledgement(result: &serde_json::Value) -> Option<Acknowledgement> {
    let [block] = result.get("content")?.as_array()?.as_slice() else {
        return None;
    };
    if block.get("type")?.as_str()? != "text" {
        return None;
    }
    read_acknowledgement(block.get("text")?.as_str()?)
}

fn read_acknowledgement(text: &str) -> Option<Acknowledgement> {
    (text.len() <= 8192)
        .then(|| serde_json::from_str(text).ok())
        .flatten()
}

fn root_turn(turn: &Turn) -> Result<Option<NativeTurnId>, AppError> {
    turn.root_turn_id
        .as_ref()
        .map(|id| id.parse().map_err(|_| AppError::Unavailable))
        .transpose()
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
    if inner.contains(['\n', '\r', ';', '&', '|', '`', '$', '<']) {
        return false;
    }
    let Some(arguments) = shlex::split(inner) else {
        return false;
    };
    let argv = if inner.contains('>') {
        match arguments.as_slice() {
            [arguments @ .., redirect, target]
                if redirect == ">" && !target.is_empty() && inner.matches('>').count() == 1 =>
            {
                arguments
            }
            _ => return false,
        }
    } else {
        &arguments
    };
    let [executable, category, operation, ..] = argv else {
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
        WorkflowTool::Report => {
            category == "report"
                && matches!(
                    operation.as_str(),
                    "complete" | "unchanged" | "needs-input" | "handoff"
                )
        }
        WorkflowTool::Action => {
            category == "action" && matches!(operation.as_str(), "declare" | "reconcile")
        }
    }
}
