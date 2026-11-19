use std::sync::Arc;

use reviewq_domain::identity::{
    DeliveryId, NativeSessionId, ReviewId, RunId, SessionKey, SubmissionId,
};
use reviewq_domain::session::{HostProvider, LeadTarget};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::hosts::{HostProof, ObservedInput};

use super::LaunchInput;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchState {
    Pending,
    Confirmed,
    Rejected,
    Uncertain,
}

pub struct LaunchReceipt {
    pub id: DeliveryId,
    pub review: ReviewId,
    pub submission: SubmissionId,
    pub state: LaunchState,
    pub run: Option<RunId>,
    pub session: Option<SessionKey>,
    pub native: Option<NativeSessionId>,
    pub job: Option<String>,
    pub observed_input: Option<ObservedInput>,
    pub provider: HostProvider,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub enum LaunchClaim {
    Existing(Snapshot<LaunchReceipt>),
    Admitted(DeliveryId),
}

pub struct ObservedLaunchSettings {
    pub model: String,
    pub effort: Option<String>,
}

pub trait LaunchStore: Send + Sync {
    fn launch_receipt(
        &self,
        actor: Actor,
        review: Option<ReviewId>,
        submission: SubmissionId,
        digest: Option<[u8; 32]>,
    ) -> AppFuture<'_, Option<Snapshot<LaunchReceipt>>>;

    fn claim_launch(
        &self,
        actor: Actor,
        input: LaunchInput,
        submission: SubmissionId,
        digest: [u8; 32],
    ) -> AppFuture<'_, LaunchClaim>;

    fn check_launch(&self, actor: Actor, id: DeliveryId) -> AppFuture<'_, ()>;

    fn record_launch_native(
        &self,
        actor: Actor,
        id: DeliveryId,
        proof: HostProof,
        job: Option<String>,
        settings: Option<ObservedLaunchSettings>,
    ) -> AppFuture<'_, ()>;

    fn record_launch_input(&self, id: DeliveryId, input: ObservedInput) -> AppFuture<'_, ()>;

    fn register_launch(
        &self,
        actor: Actor,
        id: DeliveryId,
        proof: HostProof,
        settings: Option<ObservedLaunchSettings>,
    ) -> AppFuture<'_, LeadTarget>;

    fn finish_launch(
        &self,
        id: DeliveryId,
        state: NativeLaunchResult,
    ) -> AppFuture<'_, Snapshot<LaunchReceipt>>;

    fn recover_launches(&self) -> AppFuture<'_, ()>;
}

pub trait NativeLauncher: Send + Sync {
    fn launch(
        &self,
        input: LaunchInput,
        progress: LaunchProgress,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = NativeLaunchResult> + Send + '_>>;

    fn reconcile(
        &self,
        _target: LeadTarget,
        _id: DeliveryId,
    ) -> AppFuture<'_, Option<ObservedInput>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
pub struct NativeLaunchers {
    pub codex: Option<Arc<dyn NativeLauncher>>,
    pub claude: Option<Arc<dyn NativeLauncher>>,
}

impl NativeLaunchers {
    pub fn select(&self, provider: HostProvider) -> Result<Arc<dyn NativeLauncher>, AppError> {
        match provider {
            HostProvider::Codex => self.codex.clone(),
            HostProvider::Claude => self.claude.clone(),
        }
        .ok_or(AppError::Unavailable)
    }
}

/// One durable attempt. The provider checks it at each effect boundary.
#[derive(Clone)]
pub struct LaunchProgress {
    store: Arc<dyn LaunchStore>,
    actor: Actor,
    id: DeliveryId,
}

impl LaunchProgress {
    pub fn id(&self) -> DeliveryId {
        self.id.clone()
    }

    pub async fn check(&self) -> Result<(), AppError> {
        self.store
            .check_launch(self.actor.clone(), self.id.clone())
            .await
    }

    pub async fn register(
        &self,
        proof: HostProof,
        settings: Option<ObservedLaunchSettings>,
    ) -> Result<LeadTarget, AppError> {
        self.store
            .register_launch(self.actor.clone(), self.id.clone(), proof, settings)
            .await
    }

    pub async fn record_observed_native(
        &self,
        proof: HostProof,
        job: Option<String>,
    ) -> Result<(), AppError> {
        self.store
            .record_launch_native(self.actor.clone(), self.id.clone(), proof, job, None)
            .await
    }

    pub async fn record_observed_input(&self, input: ObservedInput) -> Result<(), AppError> {
        self.store.record_launch_input(self.id.clone(), input).await
    }
}

#[derive(Clone, Copy)]
pub enum NativeLaunchResult {
    Confirmed,
    Rejected,
    Uncertain,
}

pub async fn run_launch(
    store: Arc<dyn LaunchStore>,
    host: Arc<dyn NativeLauncher>,
    actor: Actor,
    id: DeliveryId,
    input: LaunchInput,
) -> Result<Snapshot<LaunchReceipt>, AppError> {
    let progress = LaunchProgress {
        store: store.clone(),
        actor,
        id: id.clone(),
    };
    let state = host.launch(input, progress).await;
    store.finish_launch(id, state).await
}
