//! Opt-in continuation and exact managed interrupt authority.

use reviewq_domain::identity::{GitSha, NativeTurnId, ReportHash, Revision, RunId, SessionKey};
use reviewq_domain::session::LeadTarget;

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;
use crate::labels::labels;
use crate::sources::ReviewSourceReader;
use crate::workflow::{NativeWorkflowProof, ReadyWorkflow};

labels!(RecoveryKind { UsageLimit=>"usage_limit",ModelCapacity=>"model_capacity",Access=>"access" });
labels!(RecoveryState {
    Disabled=>"disabled", Waiting=>"waiting", Queued=>"queued", Started=>"started",
    Uncertain=>"uncertain", Cancelled=>"cancelled", Complete=>"complete",
});
labels!(InterruptState { Reserved=>"reserved", Interrupted=>"interrupted", Rejected=>"rejected", Uncertain=>"uncertain" });

pub struct Preferences {
    pub revision: Revision,
    pub automatic: bool,
}
#[derive(Clone)]
pub struct Episode {
    pub id: String,
    pub revision: Revision,
    pub turn: NativeTurnId,
    pub kind: RecoveryKind,
    pub state: RecoveryState,
    pub reason: Option<String>,
}
#[derive(Clone)]
pub struct Candidate {
    pub episode: Episode,
    pub lead: LeadTarget,
    pub identity: reviewq_domain::identity::MrIdentity,
    pub head: GitSha,
    pub settings: Revision,
    pub account: Option<ReportHash>,
    pub model_provider: Option<String>,
    pub prior_capacity: Option<ReportHash>,
}
pub struct NativeFailure {
    pub turn: NativeTurnId,
    pub kind: RecoveryKind,
    pub account: Option<ReportHash>,
    pub model_provider: Option<String>,
    pub at_ms: Option<i64>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryAuthorization {
    pub episode: String,
    pub failed_turn: NativeTurnId,
    pub root_turn: NativeTurnId,
    pub kind: RecoveryKind,
    pub account: ReportHash,
    pub capacity: ReportHash,
    pub model_provider: String,
}
pub struct PreparedRecovery {
    pub candidate: Candidate,
    pub authorization: RecoveryAuthorization,
    pub ready: ReadyWorkflow,
    pub reviewer: u64,
}

pub struct InterruptClaim {
    pub target: LeadTarget,
    pub turn: NativeTurnId,
    pub state: InterruptState,
    pub acquired: bool,
    pub cursor: reviewq_domain::identity::EventCursor,
}

pub trait RecoveryStore: Send + Sync {
    fn recovery_preferences(&self, actor: Actor) -> AppFuture<'_, Snapshot<Preferences>>;
    fn save_recovery_preferences(
        &self,
        actor: Actor,
        expected: Revision,
        automatic: bool,
    ) -> AppFuture<'_, Snapshot<Preferences>>;
    fn recovery_state(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
    ) -> AppFuture<'_, Snapshot<Option<Episode>>>;
    fn cancel_recovery(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
        expected: Revision,
    ) -> AppFuture<'_, Snapshot<Episode>>;
    fn due_recovery(&self) -> AppFuture<'_, Vec<Candidate>>;
    fn defer_recovery(
        &self,
        candidate: Candidate,
        reason: &'static str,
        cancel: bool,
    ) -> AppFuture<'_, ()>;
    fn reserve_recovery(&self, input: PreparedRecovery) -> AppFuture<'_, ()>;
    fn claim_interrupt(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
        turn: NativeTurnId,
    ) -> AppFuture<'_, InterruptClaim>;
    fn finish_interrupt(
        &self,
        claim: InterruptClaim,
        state: InterruptState,
    ) -> AppFuture<'_, Snapshot<InterruptState>>;
}

pub async fn run_due_recoveries(
    store: &dyn RecoveryStore,
    source: &dyn ReviewSourceReader,
    native: &dyn NativeWorkflowProof,
) -> Result<(), crate::access::AppError> {
    use crate::access::AppError;
    for candidate in store.due_recovery().await? {
        let live = source.workflow_snapshot(candidate.identity.clone()).await;
        let result = match live {
            Ok(live)
                if live.review.identity != candidate.identity
                    || live.review.head_sha != candidate.head
                    || live.review.state != crate::queue::MergeRequestState::Open =>
            {
                store
                    .defer_recovery(candidate, "recovery_source_changed", true)
                    .await
            }
            Ok(live) => match native.prepare_recovery(&candidate).await {
                Ok((authorization, ready)) => {
                    let reserved = store
                        .reserve_recovery(PreparedRecovery {
                            candidate: candidate.clone(),
                            authorization,
                            ready,
                            reviewer: live.authenticated_actor,
                        })
                        .await;
                    match reserved {
                        Err(AppError::StaleRevision | AppError::Conflict | AppError::Busy) => {
                            store
                                .defer_recovery(
                                    candidate,
                                    "recovery_waiting_for_local_eligibility",
                                    false,
                                )
                                .await
                        }
                        result => result,
                    }
                }
                Err(AppError::StaleRevision | AppError::Conflict) => {
                    store
                        .defer_recovery(candidate, "recovery_native_turn_changed", true)
                        .await
                }
                Err(_) => {
                    store
                        .defer_recovery(candidate, "recovery_native_capacity_unproved", false)
                        .await
                }
            },
            Err(_) => {
                store
                    .defer_recovery(candidate, "recovery_source_unavailable", false)
                    .await
            }
        };
        if let Err(error) = result
            && !matches!(
                error,
                AppError::StaleRevision | AppError::Busy | AppError::Conflict
            )
        {
            return Err(error);
        }
    }
    Ok(())
}
