use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use reviewq_domain::identity::{
    DeliveryId, NativeSessionId, ReviewId, RunId, SessionKey, SubmissionId,
};
use reviewq_domain::session::{AbsolutePath, HostProvider, LeadTarget};

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
    pub claude: Option<ClaudeLaunchLocator>,
    pub observed_input: Option<ObservedInput>,
    pub provider: HostProvider,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Private launch correlation recorded before starting the native job.
#[derive(Clone)]
pub struct ClaudeLaunchLocator {
    pub profile: AbsolutePath,
    pub checkout: AbsolutePath,
    pub name: Option<String>,
    pub native: Option<NativeSessionId>,
    pub job: Option<String>,
}

pub enum LaunchClaim {
    Existing(Box<Snapshot<LaunchReceipt>>),
    Admitted { id: DeliveryId, created_at_ms: i64 },
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

    fn record_claude_locator(
        &self,
        actor: Actor,
        id: DeliveryId,
        profile: AbsolutePath,
        name: String,
        job: Option<String>,
    ) -> AppFuture<'_, ()>;

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

    fn confirm_claude_launch(
        &self,
        actor: Actor,
        review: ReviewId,
        submission: SubmissionId,
        proof: HostProof,
        job: String,
    ) -> AppFuture<'_, Snapshot<LaunchReceipt>>;

    fn finish_launch(
        &self,
        id: DeliveryId,
        state: NativeLaunchResult,
    ) -> AppFuture<'_, Snapshot<LaunchReceipt>>;

    /// Removes a pending attempt the host never started, which holds no native ID, so the
    /// same submission can be admitted again: false, leaving it as it is, for any other.
    fn withdraw_launch(&self, id: DeliveryId) -> AppFuture<'_, bool>;

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

    fn reconcile_job(
        &self,
        _locator: ClaudeLaunchLocator,
        _created_at_ms: i64,
    ) -> AppFuture<'_, Option<(HostProof, String)>> {
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
    created_at_ms: i64,
}

impl LaunchProgress {
    pub fn id(&self) -> DeliveryId {
        self.id.clone()
    }

    pub fn created_at_ms(&self) -> i64 {
        self.created_at_ms
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

    pub async fn record_claude_locator(
        &self,
        profile: AbsolutePath,
        name: String,
        job: Option<String>,
    ) -> Result<(), AppError> {
        self.store
            .record_claude_locator(self.actor.clone(), self.id.clone(), profile, name, job)
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
    /// Nothing was sent to the host because ReviewQ was busy. The attempt is withdrawn, so
    /// the same submission can start it again, and the caller reads `Busy`.
    NotStarted,
}

/// The launch attempts of this process whose outcome the store does not hold yet: `None`
/// while the host works on one, and the host's result once recording it failed, as a store
/// that stays full can make it. The one owner of whether a pending attempt is in flight: a
/// replay of its submission records what is kept here, so it never waits for a restart.
static UNRECORDED: LazyLock<Mutex<HashMap<DeliveryId, Option<NativeLaunchResult>>>> =
    LazyLock::new(Default::default);

/// Keeps one attempt in `UNRECORDED` until its outcome is recorded. Dropped before then, as
/// when recording fails, it leaves the host's result there, or `Uncertain` when the host did
/// not return.
struct Flight {
    id: DeliveryId,
    result: Option<NativeLaunchResult>,
    recorded: bool,
}

impl Flight {
    fn start(id: DeliveryId) -> Self {
        if let Ok(mut unrecorded) = UNRECORDED.lock() {
            unrecorded.insert(id.clone(), None);
        }
        Self {
            id,
            result: None,
            recorded: false,
        }
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        let Ok(mut unrecorded) = UNRECORDED.lock() else {
            return;
        };
        if self.recorded {
            unrecorded.remove(&self.id);
        } else {
            let result = self.result.unwrap_or(NativeLaunchResult::Uncertain);
            unrecorded.insert(self.id.clone(), Some(result));
        }
    }
}

pub async fn run_launch(
    store: Arc<dyn LaunchStore>,
    host: Arc<dyn NativeLauncher>,
    actor: Actor,
    id: DeliveryId,
    created_at_ms: i64,
    input: LaunchInput,
) -> Result<Snapshot<LaunchReceipt>, AppError> {
    let mut flight = Flight::start(id.clone());
    let progress = LaunchProgress {
        store: store.clone(),
        actor,
        id: id.clone(),
        created_at_ms,
    };
    let result = host.launch(input, progress).await;
    flight.result = Some(result);
    let recorded = record(store.as_ref(), id, result).await;
    flight.recorded = recorded.is_ok();
    recorded?.ok_or(AppError::Busy)
}

/// Records the host's `result` for the attempt `id`: its receipt, or `None` when the host
/// started nothing and the attempt is withdrawn, so the same submission can start it again.
async fn record(
    store: &dyn LaunchStore,
    id: DeliveryId,
    result: NativeLaunchResult,
) -> Result<Option<Snapshot<LaunchReceipt>>, AppError> {
    let result = match result {
        NativeLaunchResult::NotStarted if store.withdraw_launch(id.clone()).await? => {
            return Ok(None);
        }
        // An attempt that holds a native ID may have started after all.
        NativeLaunchResult::NotStarted => NativeLaunchResult::Uncertain,
        result => result,
    };
    store.finish_launch(id, result).await.map(Some)
}

/// What a replay of a submission finds: the stored attempt `found`, or `None` when the attempt
/// was withdrawn and the submission starts again. A pending attempt this process ran without
/// recording its outcome gets that outcome recorded now; one still in flight, or that this
/// process never ran, stays as it is.
pub async fn resume_launch(
    store: &dyn LaunchStore,
    found: Snapshot<LaunchReceipt>,
) -> Result<Option<Snapshot<LaunchReceipt>>, AppError> {
    if found.value.state != LaunchState::Pending {
        return Ok(Some(found));
    }
    let id = found.value.id.clone();
    let taken = UNRECORDED.lock().ok().and_then(|mut unrecorded| {
        let result = (*unrecorded.get(&id)?)?;
        unrecorded.insert(id.clone(), None);
        Some(result)
    });
    let Some(result) = taken else {
        return Ok(Some(found));
    };
    let mut flight = Flight {
        id: id.clone(),
        result: Some(result),
        recorded: false,
    };
    let recorded = record(store, id, result).await;
    flight.recorded = recorded.is_ok();
    recorded
}
