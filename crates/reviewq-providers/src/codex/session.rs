use std::path::PathBuf;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::{HistorySession, NativeHistoryPage, NativePosition};
use reviewq_app::hosts::{
    DeliveryObservation, DispatchGuard, HostActivity, HostPhase, HostProof, MessageBody,
    NativeHost, NativeSessionMetadata, ReportRef,
};
use reviewq_domain::identity::{DeliveryId, NativeSessionId};
use reviewq_domain::session::{
    AbsolutePath, HostProvider, LeadTarget, NativeBinding, NativeIdentity,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::Codex;

#[derive(Deserialize)]
pub(super) struct ThreadRead {
    pub thread: Thread,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Thread {
    pub id: String,
    pub cwd: PathBuf,
    pub(super) parent_thread_id: Option<String>,
    source: Option<serde_json::Value>,
    pub path: Option<PathBuf>,
    pub status: ThreadStatus,
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub reasoning_effort: Option<String>,
    name: Option<String>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    created_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(super) enum ThreadStatus {
    Idle,
    NotLoaded,
    SystemError,
    Active { active_flags: Vec<String> },
}

impl NativeHost for Codex {
    fn prove(&self, native_id: NativeSessionId) -> AppFuture<'_, HostProof> {
        Box::pin(async move {
            validate_native_id(&native_id)?;
            let thread = self.read_thread(&native_id).await?;
            if thread.parent_thread_id.is_some()
                || thread
                    .source
                    .as_ref()
                    .and_then(|source| source.get("subAgent"))
                    .is_some()
            {
                return Err(AppError::Conflict);
            }
            self.proof(thread, native_id).await
        })
    }

    fn history(
        &self,
        target: HistorySession,
        position: NativePosition,
        limit: u16,
    ) -> AppFuture<'_, NativeHistoryPage> {
        Box::pin(self.read_history(target, position, limit))
    }

    fn history_content(
        &self,
        target: HistorySession,
        reference: reviewq_app::history::RpcContentRef,
        offset: u64,
        limit: u32,
    ) -> AppFuture<'_, reviewq_app::history::NativeContent> {
        Box::pin(self.read_item_content(target, reference, offset, limit))
    }

    fn history_document(
        &self,
        target: HistorySession,
        reference: reviewq_app::history::RpcContentRef,
        query: reviewq_app::history::document::Query,
    ) -> AppFuture<'_, reviewq_app::history::document::Window> {
        Box::pin(self.read_item_document(target, reference, query))
    }

    fn history_stamp(
        &self,
        target: HistorySession,
    ) -> AppFuture<'_, reviewq_app::history::NativeRpcStamp> {
        Box::pin(self.read_history_stamp(target, false))
    }

    fn history_search_stamp(
        &self,
        target: HistorySession,
    ) -> AppFuture<'_, reviewq_app::history::NativeRpcStamp> {
        Box::pin(self.read_history_stamp(target, true))
    }

    fn history_search(
        &self,
        target: HistorySession,
        input: reviewq_app::history::search::rpc::RpcSearchQuery,
    ) -> AppFuture<'_, reviewq_app::history::search::rpc::RpcSearchPage> {
        Box::pin(self.search_rpc_history(target, input))
    }

    fn history_signals(
        &self,
        target: HistorySession,
        source: Option<(AbsolutePath, reviewq_app::history::SourceAnchor)>,
        generation: String,
    ) -> AppFuture<
        '_,
        Option<tokio::sync::broadcast::Receiver<reviewq_app::history::NativeLiveSignal>>,
    > {
        Box::pin(self.open_history_signals(target, source, generation))
    }

    fn history_plan(
        &self,
        target: HistorySession,
    ) -> AppFuture<'_, reviewq_app::history::NativePlan> {
        Box::pin(self.read_current_plan(target))
    }

    fn history_navigation(
        &self,
        target: HistorySession,
        query: reviewq_app::history::NavigationQuery,
    ) -> AppFuture<'_, reviewq_app::history::NativeNavigation> {
        Box::pin(self.read_prompt_navigation(target, query))
    }

    fn history_around(
        &self,
        target: HistorySession,
        id: reviewq_domain::identity::NativeInputId,
        anchor: Option<(NativePosition, String)>,
        limit: u16,
    ) -> AppFuture<'_, NativeHistoryPage> {
        Box::pin(self.read_around_item(target, id, anchor, limit))
    }

    fn interruptible_turn(
        &self,
        target: LeadTarget,
    ) -> AppFuture<'_, Option<reviewq_domain::identity::NativeTurnId>> {
        Box::pin(async move { self.active_managed_turn(&target).await })
    }

    fn observe_interrupt(
        &self,
        target: LeadTarget,
        turn: reviewq_domain::identity::NativeTurnId,
    ) -> AppFuture<'_, bool> {
        Box::pin(self.observe_exact_interrupt(target, turn))
    }

    fn interrupt_managed(
        &self,
        target: LeadTarget,
        turn: reviewq_domain::identity::NativeTurnId,
    ) -> AppFuture<'_, reviewq_app::hosts::recovery::InterruptState> {
        Box::pin(self.stop_exact_turn(target, turn))
    }

    fn dispatch(
        &self,
        target: LeadTarget,
        delivery: DeliveryId,
        text: MessageBody,
        report: Option<ReportRef>,
        guard: DispatchGuard,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DeliveryObservation> + Send + '_>> {
        Box::pin(self.send_to_thread(target, delivery, text, report, guard))
    }

    fn observe(
        &self,
        target: LeadTarget,
        delivery: DeliveryId,
        cursor: Option<String>,
        require_new_turn: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DeliveryObservation> + Send + '_>> {
        Box::pin(self.find_input(target, delivery, cursor, require_new_turn))
    }
}

impl Codex {
    pub(super) async fn proof(
        &self,
        thread: Thread,
        native_id: NativeSessionId,
    ) -> Result<HostProof, AppError> {
        self.prove_thread(thread, native_id, None).await
    }

    pub(super) async fn prove_thread(
        &self,
        thread: Thread,
        native_id: NativeSessionId,
        parent: Option<reviewq_app::history::HistoryParent>,
    ) -> Result<HostProof, AppError> {
        validate_native_id(&native_id)?;
        if thread.id != native_id.as_str()
            || thread.parent_thread_id.as_deref()
                != parent.as_ref().map(|owner| owner.native_id.as_str())
        {
            return Err(AppError::Conflict);
        }
        let metadata = thread.metadata();
        let cwd = tokio::fs::canonicalize(&thread.cwd)
            .await
            .unwrap_or(thread.cwd);
        let cwd = AbsolutePath::from_canonical(cwd).map_err(|_| AppError::Unavailable)?;
        let binding = NativeBinding::new(
            NativeIdentity::new(HostProvider::Codex, self.profile.clone(), native_id),
            cwd,
        );
        let source = match thread.path {
            Some(path) => {
                crate::history::verified_codex_source(binding.clone(), path, parent).await?
            }
            None => None,
        };
        Ok(HostProof {
            binding,
            source,
            activity: thread.status.activity(),
            metadata,
        })
    }

    pub(super) async fn verified_thread(&self, lead: &LeadTarget) -> Result<Thread, AppError> {
        let expected = lead.binding().identity();
        if expected.profile() != &self.profile || expected.provider() != HostProvider::Codex {
            return Err(AppError::NotFound);
        }
        let thread = self.read_thread(expected.native_id()).await?;
        if canonical_cwd(thread.cwd.clone()).await? != *lead.binding().cwd() {
            return Err(AppError::Conflict);
        }
        Ok(thread)
    }

    pub(super) async fn retained_thread(&self, lead: &LeadTarget) -> Result<Thread, AppError> {
        let expected = lead.binding().identity();
        if expected.profile() != &self.profile || expected.provider() != HostProvider::Codex {
            return Err(AppError::NotFound);
        }
        let thread = self.read_thread(expected.native_id()).await?;
        if !retained_cwd(&thread, lead.binding().cwd()).await? {
            return Err(AppError::Conflict);
        }
        Ok(thread)
    }

    pub(super) async fn bound_history_thread(
        &self,
        session: &HistorySession,
    ) -> Result<Thread, AppError> {
        let identity = session.binding.identity();
        if identity.provider() != HostProvider::Codex || identity.profile() != &self.profile {
            return Err(AppError::NotFound);
        }
        let thread = self.read_thread(identity.native_id()).await?;
        if thread.parent_thread_id.as_deref()
            != session
                .parent
                .as_ref()
                .map(|parent| parent.native_id.as_str())
            || !retained_cwd(&thread, session.binding.cwd()).await?
        {
            return Err(AppError::Conflict);
        }
        Ok(thread)
    }

    pub(super) async fn read_thread(
        &self,
        native_id: &NativeSessionId,
    ) -> Result<Thread, AppError> {
        let found: ThreadRead = self
            .connection
            .read(
                "thread/read",
                json!({"threadId":native_id.as_str(),"includeTurns":false}),
            )
            .await?;
        if found.thread.id != native_id.as_str() {
            return Err(AppError::Conflict);
        }
        Ok(found.thread)
    }
}

pub(super) async fn retained_cwd(
    thread: &Thread,
    expected: &AbsolutePath,
) -> Result<bool, AppError> {
    if thread.cwd == expected.as_path() {
        return Ok(true);
    }
    Ok(canonical_cwd(thread.cwd.clone()).await? == *expected)
}

pub(super) async fn canonical_cwd(path: PathBuf) -> Result<AbsolutePath, AppError> {
    let cwd = tokio::fs::canonicalize(path)
        .await
        .map_err(|_| AppError::Unavailable)?;
    if !tokio::fs::metadata(&cwd)
        .await
        .map_err(|_| AppError::Unavailable)?
        .is_dir()
    {
        return Err(AppError::Unavailable);
    }
    AbsolutePath::from_canonical(cwd).map_err(|_| AppError::Unavailable)
}

fn validate_native_id(id: &NativeSessionId) -> Result<(), AppError> {
    let value = Uuid::parse_str(id.as_str()).map_err(|_| AppError::InvalidRequest)?;
    if value.is_nil() || value.hyphenated().to_string() != id.as_str() {
        return Err(AppError::InvalidRequest);
    }
    Ok(())
}

impl Thread {
    pub(super) fn metadata(&self) -> NativeSessionMetadata {
        use crate::settings::safe_label;
        NativeSessionMetadata {
            name: [&self.name, &self.agent_nickname, &self.agent_role]
                .into_iter()
                .find_map(|value| safe_label(value.as_deref()?, 512).ok()),
            status: Some(self.status.label().to_owned()),
            started_at_ms: self
                .created_at
                .filter(|time| *time > 0)
                .and_then(|time| time.checked_mul(1000)),
            model: self
                .model
                .as_deref()
                .and_then(|value| safe_label(value, 256).ok()),
            effort: self
                .reasoning_effort
                .as_deref()
                .and_then(|value| safe_label(value, 64).ok()),
        }
    }
}

impl ThreadStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::NotLoaded => "not_loaded",
            Self::SystemError => "system_error",
            Self::Active { active_flags } if active_flags.is_empty() => "running",
            Self::Active { .. } => "waiting",
        }
    }

    pub(super) fn activity(&self) -> Option<HostActivity> {
        let (phase, has_questions) = match self {
            Self::Idle => (HostPhase::Idle, false),
            Self::SystemError => (HostPhase::Failed, false),
            Self::NotLoaded => return None,
            Self::Active { active_flags } => {
                let phase = if active_flags.is_empty() {
                    HostPhase::Working
                } else {
                    HostPhase::Blocked
                };
                (
                    phase,
                    active_flags.iter().any(|flag| flag == "waitingOnUserInput"),
                )
            }
        };
        Some(HostActivity {
            phase,
            has_questions,
        })
    }
}
