use std::path::PathBuf;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::hosts::ObservedInput;
use reviewq_app::launch::{
    Invocation, LaunchInput, LaunchProgress, NativeLaunchResult, NativeLauncher,
    ObservedLaunchSettings,
};
use reviewq_domain::identity::DeliveryId;
use reviewq_domain::session::{HostProvider, LeadTarget};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Codex;
use super::connection::CallError;
use super::delivery::proved_new_turn;
use super::history::Item;
use super::session::{Thread, ThreadStatus, canonical_cwd};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartedThread {
    thread: Thread,
    cwd: PathBuf,
    model: String,
    reasoning_effort: Option<String>,
}

impl NativeLauncher for Codex {
    fn launch(
        &self,
        input: LaunchInput,
        progress: LaunchProgress,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = NativeLaunchResult> + Send + '_>> {
        Box::pin(self.start_review(input, progress))
    }

    fn reconcile(
        &self,
        target: LeadTarget,
        id: DeliveryId,
    ) -> AppFuture<'_, Option<ObservedInput>> {
        Box::pin(self.launch_input(target, id))
    }
}

impl Codex {
    async fn start_review(
        &self,
        input: LaunchInput,
        progress: LaunchProgress,
    ) -> NativeLaunchResult {
        if canonical_cwd(input.checkout.root.as_path().to_owned())
            .await
            .as_ref()
            .ok()
            != Some(&input.checkout.root)
        {
            return NativeLaunchResult::Rejected;
        }
        // A new thread reads Codex's config from disk, so ReviewQ's hooks are trusted first.
        let _ = self.approve_own_hooks().await;
        let mut params = json!({"cwd":input.checkout.root.as_path(),"ephemeral":false});
        if let Some(model) = &input.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = &input.effort {
            params["config"] = json!({"model_reasoning_effort":effort});
        }
        let StartedThread {
            thread,
            cwd,
            model,
            reasoning_effort,
        } = match self
            .connection
            .launch("thread/start", params, progress.clone())
            .await
        {
            Ok(value) => value,
            // Nothing was sent: a busy ReviewQ leaves the launch to be started again, and any
            // other refusal ends it.
            Err(CallError::BeforeSend(AppError::Busy) | CallError::Denied(AppError::Busy)) => {
                return NativeLaunchResult::NotStarted;
            }
            Err(CallError::BeforeSend(_) | CallError::Denied(_)) => {
                return NativeLaunchResult::Rejected;
            }
            Err(_) => return NativeLaunchResult::Uncertain,
        };
        let Ok(native) = thread.id.parse() else {
            return NativeLaunchResult::Uncertain;
        };
        let Ok(proof) = self.proof(thread, native).await else {
            return NativeLaunchResult::Uncertain;
        };
        if proof.binding.cwd() != &input.checkout.root || cwd != input.checkout.root.as_path() {
            return NativeLaunchResult::Uncertain;
        }
        let observed = ObservedLaunchSettings {
            model: model.clone(),
            effort: reasoning_effort.clone(),
        };
        let Ok(lead) = progress.register(proof, Some(observed)).await else {
            return NativeLaunchResult::Uncertain;
        };
        if model.is_empty()
            || input
                .model
                .as_ref()
                .is_some_and(|expected| expected != &model)
            || input
                .effort
                .as_ref()
                .is_some_and(|expected| reasoning_effort.as_ref() != Some(expected))
        {
            return NativeLaunchResult::Uncertain;
        }
        let native = lead.binding().identity().native_id();
        let name: String = format!(
            "{}!{} {}",
            input.source.project_path,
            input.source.identity.iid(),
            input.source.title
        )
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect();
        let _: Result<Value, _> = self
            .connection
            .launch(
                "thread/name/set",
                json!({"threadId":native.as_str(),"name":name}),
                progress.clone(),
            )
            .await;
        if !self
            .verified_thread(&lead)
            .await
            .is_ok_and(|thread| matches!(thread.status, ThreadStatus::Idle))
        {
            return NativeLaunchResult::Uncertain;
        }
        if !self
            .turns(native.as_str(), None, true, 1, false)
            .await
            .is_ok_and(|page| page.data.is_empty())
        {
            return NativeLaunchResult::Uncertain;
        }
        let delivery = progress.id();
        let mut items = vec![json!({"type":"text","text":input.prompt(),"text_elements":[]})];
        if let Invocation::Skill { skill, .. } = &input.invocation {
            items.push(json!({"type":"skill","name":skill.name,"path":skill.path.as_path()}));
        }
        let mut params = json!({
            "threadId":native.as_str(), "input":items, "clientUserMessageId":delivery.as_str(),
            "turnTrigger":format!("reviewq:{delivery}"),
        });
        if let Some(model) = input.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = input.effort {
            params["effort"] = json!(effort);
        }
        let _: Result<Value, _> = self
            .connection
            .launch("turn/start", params, progress.clone())
            .await;
        if let Ok(Some(input)) = self.launch_input(lead, delivery).await
            && progress.record_observed_input(input).await.is_ok()
        {
            return NativeLaunchResult::Confirmed;
        }
        NativeLaunchResult::Uncertain
    }

    async fn launch_input(
        &self,
        lead: LeadTarget,
        id: DeliveryId,
    ) -> Result<Option<ObservedInput>, AppError> {
        let identity = lead.binding().identity();
        if identity.provider() != HostProvider::Codex || identity.profile() != &self.profile {
            return Ok(None);
        }
        let thread = self.read_thread(identity.native_id()).await?;
        if thread.cwd != lead.binding().cwd().as_path()
            && canonical_cwd(thread.cwd.clone()).await.as_ref().ok() != Some(lead.binding().cwd())
        {
            return Ok(None);
        }
        let mut turns = self.turns(&thread.id, None, true, 1, true).await?;
        let Some(turn) = turns.data.pop() else {
            return Ok(None);
        };
        let Some(entry) = turn.items.into_iter().find_map(|item| match item {
            Item::UserMessage {
                id: entry,
                client_id: Some(marker),
                ..
            } if marker == id.as_str() => Some(entry),
            _ => None,
        }) else {
            return Ok(None);
        };
        let (Ok(turn_id), Ok(entry_id)) = (turn.id.parse(), entry.parse()) else {
            return Ok(None);
        };
        if !proved_new_turn(&lead, &thread.path, turn.id, &id).await {
            return Ok(None);
        }
        Ok(Some(ObservedInput { turn_id, entry_id }))
    }
}
