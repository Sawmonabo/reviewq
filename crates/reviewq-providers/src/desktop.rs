use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use reviewq_app::access::AppFuture;
use reviewq_app::notifications::{DesktopClaim, DesktopNotifier, DesktopOutcome};
use tokio::process::Command;
use url::Url;

mod windows;
pub use windows::{
    WindowsDesktop, WindowsDesktopNotifier, WindowsRegistration, open_windows_browser,
};

/// Linux desktop delivery via the installed notification client.
pub struct LinuxDesktopNotifier {
    browser: SocketAddr,
    executable: PathBuf,
}

impl LinuxDesktopNotifier {
    pub fn new(browser: SocketAddr, executable: PathBuf) -> Option<Self> {
        if !browser.ip().is_loopback() || !executable.is_absolute() {
            return None;
        }
        Some(Self {
            browser,
            executable,
        })
    }
}

impl DesktopNotifier for LinuxDesktopNotifier {
    fn notify<'a>(&'a self, claim: &'a DesktopClaim) -> AppFuture<'a, DesktopOutcome> {
        Box::pin(async move {
            let message = Message::new(self.browser, claim);
            let body = format!(
                "{}\n<a href=\"{}\">Open ReviewQ</a>",
                escape(&message.body),
                escape(message.url.as_str())
            );
            let spawned = Command::new(&self.executable)
                .args([
                    "--app-name=ReviewQ",
                    "--urgency=normal",
                    "--",
                    "ReviewQ",
                    &body,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn();
            let mut child = match spawned {
                Ok(child) => child,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(DesktopOutcome::NotStarted("desktop_client_unavailable"));
                }
                Err(_) => return Ok(DesktopOutcome::NotStarted("desktop_client_failed_to_start")),
            };
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) if status.success() => Ok(DesktopOutcome::Delivered),
                _ => Ok(DesktopOutcome::Uncertain(
                    "desktop_acknowledgement_unavailable",
                )),
            }
        })
    }
}

struct Message {
    url: Url,
    body: String,
}

impl Message {
    fn new(browser: SocketAddr, claim: &DesktopClaim) -> Self {
        let mut url =
            Url::parse(&format!("http://{browser}/")).expect("a loopback address is a valid URI");
        let id = claim.notification.as_str();
        url.query_pairs_mut()
            .append_pair("view", "notifications")
            .append_pair("notificationId", id);
        url.set_fragment(Some(&format!("notification-{id}")));
        let label: String = claim
            .label
            .chars()
            .filter(|ch| !ch.is_control())
            .take(120)
            .collect();
        let body = if claim.event_count > 1 {
            format!("{label}\n{} updates", claim.event_count)
        } else {
            label
        };
        Self { url, body }
    }
}

fn escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
