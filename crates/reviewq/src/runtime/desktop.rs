use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use reviewq_app::access::AppFuture;
use reviewq_app::notifications::{
    DesktopClaim, DesktopNotifier, DesktopOutcome, DesktopStore, dispatch_desktop,
};
use reviewq_providers::{LinuxDesktopNotifier, WindowsDesktop, WindowsDesktopNotifier};
use reviewq_store::SqliteStore;
use reviewq_transport::{BrowserDesktop, BrowserOffer};
use tokio::task::JoinHandle;

struct Desktop {
    store: Arc<SqliteStore>,
    browser: BrowserDesktop,
    native: Option<Box<dyn DesktopNotifier>>,
}

pub(super) fn start(
    store: Arc<SqliteStore>,
    address: SocketAddr,
    browser: BrowserDesktop,
    windows: Result<Option<WindowsDesktop>, super::Failure>,
) -> JoinHandle<()> {
    let native: Option<Box<dyn DesktopNotifier>> = match windows {
        Ok(Some(windows)) => WindowsDesktopNotifier::new(address, windows)
            .map(|sender| Box::new(sender) as Box<dyn DesktopNotifier>),
        Ok(None) => {
            let executable = crate::runtime_settings::executable("notify-send")
                .unwrap_or_else(|| PathBuf::from("/usr/bin/notify-send"));
            LinuxDesktopNotifier::new(address, executable)
                .map(|sender| Box::new(sender) as Box<dyn DesktopNotifier>)
        }
        Err(error) => {
            eprintln!("ReviewQ native notification fallback unavailable ({error}).");
            None
        }
    };
    let notifier = Desktop {
        store: store.clone(),
        browser,
        native,
    };
    let mut commits = store.subscribe_desktop();
    tokio::spawn(async move {
        loop {
            commits.borrow_and_update();
            match dispatch_desktop(store.as_ref(), &notifier).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            }
            let due = match store.next_desktop_due().await {
                Ok(due) => due,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            tokio::select! {
                result = commits.changed() => if result.is_err() { break; },
                () = super::wait_until_due(due) => {},
            }
        }
    })
}

impl DesktopNotifier for Desktop {
    fn notify<'a>(&'a self, claim: &'a DesktopClaim) -> AppFuture<'a, DesktopOutcome> {
        Box::pin(async move {
            let offer = self.browser.offer(claim).await;
            let refreshed;
            let current = if matches!(offer, BrowserOffer::Unavailable) {
                claim
            } else {
                let Some(current) = self.store.refresh_desktop_claim(claim.clone()).await? else {
                    return Ok(DesktopOutcome::NotStarted("notification_no_longer_unread"));
                };
                refreshed = current;
                &refreshed
            };
            // A changed representative or member count requires fresh desktop text.
            if let BrowserOffer::Accepted(display) = offer
                && current.notification == claim.notification
                && current.event_count == claim.event_count
                && current.label == claim.label
                && let Some(result) = display.show().await
            {
                return Ok(result);
            }
            match &self.native {
                Some(native) => native.notify(current).await,
                None => Ok(DesktopOutcome::NotStarted("desktop_client_unavailable")),
            }
        })
    }
}
