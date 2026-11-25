use std::path::Path;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::hosts::DispatchGuard;
use reviewq_app::workflow::{
    NativeCall, NativeWorkflowProof, ReadyWorkflow, WorkflowHold, WorkflowReadiness,
};
use reviewq_domain::identity::{NativeInputId, NativeTurnId};
use reviewq_domain::session::LeadTarget;

use super::Codex;
use super::history::{Item, Turn};
use super::session::{Thread, ThreadStatus};

#[cfg(test)]
mod tests;

impl NativeWorkflowProof for Codex {
    fn dispatch_readiness(&self, lead: LeadTarget) -> AppFuture<'_, WorkflowReadiness> {
        Box::pin(async move { self.dispatch_thread_readiness(&lead, false).await })
    }

    fn followup_readiness(&self, lead: LeadTarget) -> AppFuture<'_, WorkflowReadiness> {
        Box::pin(async move { self.dispatch_thread_readiness(&lead, true).await })
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
    ) -> AppFuture<'_, NativeCall> {
        Box::pin(async move {
            let (thread, active) = self.active_workflow_turn(&lead, turn.as_ref()).await?;
            let index = call_position(&active, &call_id).ok_or(AppError::NotFound)?;
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
                call_id,
                source_generation: generation,
                source_position: position,
                request_entry_id: request_address(&active, index)?,
            })
        })
    }
}

impl Codex {
    async fn dispatch_thread_readiness(
        &self,
        lead: &LeadTarget,
        followup: bool,
    ) -> Result<WorkflowReadiness, AppError> {
        let Ok(thread) = self.verified_thread(lead).await else {
            return Ok(WorkflowReadiness::Held(WorkflowHold::ProviderUnavailable));
        };
        if followup {
            self.followup_thread_readiness(lead, &thread).await
        } else {
            self.idle_readiness(lead, &thread).await
        }
    }

    async fn followup_thread_readiness(
        &self,
        lead: &LeadTarget,
        thread: &Thread,
    ) -> Result<WorkflowReadiness, AppError> {
        use WorkflowReadiness::{Held, Ready};
        if !thread.is_lead() {
            return Ok(Held(WorkflowHold::ProviderUnavailable));
        }
        if !matches!(thread.status, ThreadStatus::NotLoaded) {
            return self.idle_readiness(lead, thread).await;
        }
        // The daemon can attest to the saved turn without loading the session.
        let readiness = self.workflow_context(lead, thread, None).await?;
        if matches!(&readiness, Ready(ready) if ready.latest_user_turn_id.is_none()) {
            return Ok(Held(WorkflowHold::UnknownSettings));
        }
        Ok(readiness)
    }

    pub(super) async fn resume_saved_followup(
        &self,
        lead: &LeadTarget,
        thread: &Thread,
        guard: &DispatchGuard,
    ) -> Result<Thread, AppError> {
        let expected = &guard.workflow().ok_or(AppError::Forbidden)?.ready;
        self.verify_saved_followup(lead, thread, expected).await?;
        let current = self.verified_thread(lead).await?;
        if !current.is_lead()
            || current.path != thread.path
            || !matches!(current.status, ThreadStatus::NotLoaded)
        {
            return Err(AppError::StaleRevision);
        }
        let resumed = self
            .resume_exact_thread(lead, &current, Some(guard.followup_resume()?))
            .await?;
        verify_resumed_followup(thread, &resumed)?;
        Ok(resumed)
    }

    async fn verify_saved_followup(
        &self,
        lead: &LeadTarget,
        thread: &Thread,
        expected: &ReadyWorkflow,
    ) -> Result<(), AppError> {
        if !matches!(thread.status, ThreadStatus::NotLoaded)
            || !matches!(self.followup_thread_readiness(lead, thread).await?,
                WorkflowReadiness::Ready(ready) if ready == *expected)
        {
            return Err(AppError::StaleRevision);
        }
        Ok(())
    }

    async fn active_workflow_turn(
        &self,
        lead: &LeadTarget,
        turn: Option<&NativeTurnId>,
    ) -> Result<(Thread, Turn), AppError> {
        let thread = self.verified_thread(lead).await?;
        if !matches!(thread.status, ThreadStatus::Active { .. }) {
            return Err(AppError::NotFound);
        }
        let active = self
            .turns(&thread.id, None, false, 1, true)
            .await?
            .data
            .pop()
            .filter(|native| {
                native.status == "inProgress" && turn.is_none_or(|turn| native.id == turn.as_str())
            })
            .ok_or(AppError::NotFound)?;
        Ok((thread, active))
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
        use WorkflowReadiness::Held;
        match &thread.status {
            ThreadStatus::Idle => {}
            ThreadStatus::NotLoaded => return Ok(Held(WorkflowHold::SessionNotLoaded)),
            ThreadStatus::Active { active_flags } if active_flags.is_empty() => {
                return Ok(Held(WorkflowHold::ActiveWork));
            }
            ThreadStatus::Active { .. } => return Ok(Held(WorkflowHold::PendingNativeInput)),
            ThreadStatus::SystemError => return Ok(Held(WorkflowHold::ProviderUnavailable)),
        }
        self.workflow_context(lead, thread, failed).await
    }

    async fn workflow_context(
        &self,
        lead: &LeadTarget,
        thread: &Thread,
        failed: Option<&NativeTurnId>,
    ) -> Result<WorkflowReadiness, AppError> {
        use WorkflowReadiness::{Held, Ready};
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

fn verify_resumed_followup(saved: &Thread, resumed: &Thread) -> Result<(), AppError> {
    if !resumed.is_lead()
        || resumed.path != saved.path
        || !matches!(resumed.status, ThreadStatus::Idle)
    {
        return Err(AppError::Conflict);
    }
    Ok(())
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

fn call_position(turn: &Turn, call_id: &str) -> Option<usize> {
    turn.items.iter().position(|item| matches!(item,
        Item::CommandExecution { id, command, source, .. }
            if id == call_id && matches!(source.as_deref(), Some("agent" | "unifiedExecStartup")) && action_command(command)))
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

/// Whether `command` runs `reviewq action declare` or `reviewq action reconcile`, alone or
/// through one shell, with at most one output redirect.
fn action_command(command: &str) -> bool {
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
    category == "action" && matches!(operation.as_str(), "declare" | "reconcile")
}
