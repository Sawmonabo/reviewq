use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use reviewq_app::notifications::{DesktopNotifier, DesktopStore, dispatch_desktop};
use reviewq_providers::{LinuxDesktopNotifier, WindowsDesktop, WindowsDesktopNotifier};
use reviewq_store::SqliteStore;
use tokio::task::JoinHandle;

pub(super) fn start(
    store: Arc<SqliteStore>,
    browser: SocketAddr,
    windows: Option<WindowsDesktop>,
) -> Option<JoinHandle<()>> {
    let notifier: Box<dyn DesktopNotifier> = if let Some(windows) = windows {
        Box::new(WindowsDesktopNotifier::new(browser, windows)?)
    } else {
        let executable = crate::runtime_settings::executable("notify-send")
            .unwrap_or_else(|| PathBuf::from("/usr/bin/notify-send"));
        Box::new(LinuxDesktopNotifier::new(browser, executable)?)
    };
    let mut commits = store.subscribe_desktop();
    Some(tokio::spawn(async move {
        loop {
            commits.borrow_and_update();
            match dispatch_desktop(store.as_ref(), notifier.as_ref()).await {
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
    }))
}
