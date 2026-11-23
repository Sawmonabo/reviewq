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

impl DesktopClaim {
    pub fn body(&self) -> String {
        let label: String = self
            .label
            .chars()
            .filter(|ch| !ch.is_control())
            .take(120)
            .collect();
        if self.event_count > 1 {
            format!("{label}\n{} updates", self.event_count)
        } else {
            label
        }
    }
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
    fn next_desktop_due(&self) -> AppFuture<'_, Option<i64>>;
}

pub trait DesktopNotifier: Send + Sync {
    fn notify<'a>(&'a self, claim: &'a DesktopClaim) -> AppFuture<'a, DesktopOutcome>;
}

pub async fn dispatch_desktop(
    store: &dyn DesktopStore,
    notifier: &dyn DesktopNotifier,
) -> Result<bool, AppError> {
    let Some(claim) = store.claim_desktop().await? else {
        return Ok(false);
    };
    let Some(current) = store.refresh_desktop_claim(claim.clone()).await? else {
        store
            .finish_desktop(
                claim,
                DesktopOutcome::NotStarted("notification_no_longer_unread"),
            )
            .await?;
        return Ok(true);
    };
    let result = match notifier.notify(&current).await {
        Ok(result) => result,
        Err(_) => DesktopOutcome::Uncertain("notification_bridge_unknown"),
    };
    store.finish_desktop(current, result).await?;
    Ok(true)
}
