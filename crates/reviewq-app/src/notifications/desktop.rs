use reviewq_domain::identity::{NotificationId, ReviewId, RunId, SessionKey};

use crate::access::{AppError, AppFuture};

use super::Kind;

#[derive(Clone)]
pub struct DesktopClaim {
    pub id: String,
    pub fence: u64,
    pub notification: NotificationId,
    pub review: ReviewId,
    pub run: Option<RunId>,
    pub session: Option<SessionKey>,
    pub kind: Kind,
    pub label: String,
    pub event_count: u16,
}

pub enum DesktopOutcome {
    Delivered,
    NotStarted(&'static str),
    Uncertain(&'static str),
}

pub trait DesktopStore: Send + Sync {
    fn recover_desktop_claims(&self) -> AppFuture<'_, ()>;
    fn claim_desktop(&self) -> AppFuture<'_, Option<DesktopClaim>>;
    fn refresh_desktop_claim(&self, claim: DesktopClaim) -> AppFuture<'_, Option<DesktopClaim>>;
    fn finish_desktop(&self, claim: DesktopClaim, result: DesktopOutcome) -> AppFuture<'_, ()>;
}

pub trait DesktopNotifier: Send + Sync {
    fn notify<'a>(&'a self, claim: &'a DesktopClaim) -> AppFuture<'a, DesktopOutcome>;
}

pub async fn dispatch_desktop(
    store: &dyn DesktopStore,
    notifier: &dyn DesktopNotifier,
) -> Result<(), AppError> {
    let Some(claim) = store.claim_desktop().await? else {
        return Ok(());
    };
    let Some(current) = store.refresh_desktop_claim(claim.clone()).await? else {
        return store
            .finish_desktop(
                claim,
                DesktopOutcome::NotStarted("notification_no_longer_unread"),
            )
            .await;
    };
    let result = match notifier.notify(&current).await {
        Ok(result) => result,
        Err(_) => DesktopOutcome::Uncertain("notification_bridge_unknown"),
    };
    store.finish_desktop(current, result).await
}
