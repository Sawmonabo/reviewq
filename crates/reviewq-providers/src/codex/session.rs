use std::path::PathBuf;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::{NativeHistoryPage, NativePosition};
use reviewq_app::hosts::{DeliveryObservation, DispatchGuard, MessageBody, NativeHost};
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
    pub path: Option<PathBuf>,
    pub status: ThreadStatus,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
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
    fn prove(&self, native_id: NativeSessionId) -> AppFuture<'_, NativeBinding> {
        Box::pin(async move {
            validate_native_id(&native_id)?;
            let thread = self.read_thread(&native_id).await?;
            Ok(NativeBinding::new(
                NativeIdentity::new(HostProvider::Codex, self.profile.clone(), native_id),
                canonical_cwd(thread.cwd).await?,
            ))
        })
    }

    fn history(
        &self,
        target: LeadTarget,
        position: NativePosition,
        limit: u16,
    ) -> AppFuture<'_, NativeHistoryPage> {
        Box::pin(self.read_history(target, position, limit))
    }

    fn dispatch(
        &self,
        target: LeadTarget,
        delivery: DeliveryId,
        text: MessageBody,
        guard: DispatchGuard,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DeliveryObservation> + Send + '_>> {
        Box::pin(self.send_to_thread(target, delivery, text, guard))
    }

    fn observe(
        &self,
        target: LeadTarget,
        delivery: DeliveryId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DeliveryObservation> + Send + '_>> {
        Box::pin(self.find_input(target, delivery))
    }
}

impl Codex {
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

    async fn read_thread(&self, native_id: &NativeSessionId) -> Result<Thread, AppError> {
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
