use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use reviewq_app::access::AppFuture;
use reviewq_app::notifications::{DesktopClaim, DesktopNotifier, DesktopOutcome};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use url::Url;

use super::Message;

const OPEN_BROWSER_SCRIPT: &str = r"
$ErrorActionPreference = 'Stop'
$start = [Diagnostics.ProcessStartInfo]::new([Console]::In.ReadToEnd())
$start.UseShellExecute = $true
[void][Diagnostics.Process]::Start($start)
";

pub struct WindowsDesktop {
    executable: PathBuf,
    owner: String,
}

pub enum WindowsRegistration {
    Check,
    Install,
    Remove,
}

/// Open the verified dashboard URL through Windows. `None` means PowerShell could not start.
pub async fn open_windows_browser(url: &Url) -> Option<bool> {
    let executable = windows_tool()?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Some(false);
    }
    let child = start_powershell(&executable, OPEN_BROWSER_SCRIPT).ok()?;
    Some(
        send_to_powershell(child, url.as_str())
            .await
            .is_ok_and(|status| status.success()),
    )
}

impl WindowsDesktop {
    pub fn available() -> bool {
        windows_tool().is_some()
    }

    pub fn new(owner: String) -> Option<Self> {
        if owner.len() != 64 || !owner.bytes().all(|ch| ch.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self {
            executable: windows_tool()?,
            owner,
        })
    }

    pub async fn registration(
        &self,
        operation: WindowsRegistration,
        digest: &str,
        previous: Option<&str>,
        icon: Option<&[u8]>,
    ) -> io::Result<()> {
        let operation = match operation {
            WindowsRegistration::Check => "check",
            WindowsRegistration::Install => "install",
            WindowsRegistration::Remove => "remove",
        };
        let status = self
            .run(json!({
                "operation": operation,
                "digest": digest,
                "previous": previous,
                "icon": icon.map(|icon| STANDARD.encode(icon)),
            }))
            .await?;
        match status.code() {
            Some(0) => Ok(()),
            Some(22) => Err(io::Error::other(
                "Preserved a conflicting Personal.ReviewQ identity or icon.",
            )),
            _ => Err(io::Error::other(
                "Windows could not verify the Personal.ReviewQ notification identity.",
            )),
        }
    }

    pub async fn test(&self, browser: SocketAddr) -> DesktopOutcome {
        self.show(
            browser,
            "ReviewQ test notification",
            "ReviewQ can reach your desktop.",
            "test",
        )
        .await
    }

    /// Shows one notification with `title` and `body` that opens the dashboard at `browser`
    /// when clicked. A later notification with the same `key` replaces it.
    pub async fn show(
        &self,
        browser: SocketAddr,
        title: &str,
        body: &str,
        key: &str,
    ) -> DesktopOutcome {
        self.send(&format!("http://{browser}/"), title, body, &tag(key))
            .await
    }

    async fn send(&self, url: &str, title: &str, body: &str, tag: &str) -> DesktopOutcome {
        let response = self
            .run(json!({
                "operation": "show",
                "url": url,
                "title": title,
                "body": body,
                "tag": tag,
            }))
            .await;
        match response {
            Ok(status) if status.success() => DesktopOutcome::Delivered,
            Ok(status) if status.code() == Some(20) => {
                DesktopOutcome::NotStarted("windows_notification_unavailable")
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                DesktopOutcome::NotStarted("windows_notification_unavailable")
            }
            _ => DesktopOutcome::Uncertain("windows_notification_acknowledgement_unavailable"),
        }
    }

    async fn run(&self, mut body: Value) -> io::Result<ExitStatus> {
        body["owner"] = json!(self.owner);
        let payload = STANDARD.encode(serde_json::to_vec(&body)?);
        let child = start_powershell(&self.executable, include_str!("windows.ps1"))?;
        send_to_powershell(child, &payload).await
    }
}

pub struct WindowsDesktopNotifier {
    browser: SocketAddr,
    sender: WindowsDesktop,
}

impl WindowsDesktopNotifier {
    pub fn new(browser: SocketAddr, sender: WindowsDesktop) -> Option<Self> {
        browser
            .ip()
            .is_loopback()
            .then_some(Self { browser, sender })
    }
}

impl DesktopNotifier for WindowsDesktopNotifier {
    fn notify<'a>(&'a self, claim: &'a DesktopClaim) -> AppFuture<'a, DesktopOutcome> {
        Box::pin(async move {
            let message = Message::new(self.browser, claim);
            Ok(self
                .sender
                .send(
                    message.url.as_str(),
                    "ReviewQ",
                    &message.body,
                    &tag(claim.review.as_str()),
                )
                .await)
        })
    }
}

fn tag(key: &str) -> String {
    Sha256::digest(key.as_bytes())[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn start_powershell(executable: &Path, script: &str) -> io::Result<Child> {
    let script: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    Command::new(executable)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &STANDARD.encode(script),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
}

async fn send_to_powershell(mut child: Child, input: &str) -> io::Result<ExitStatus> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("Windows input unavailable."))?;
    tokio::time::timeout(Duration::from_secs(15), async move {
        stdin.write_all(input.as_bytes()).await?;
        drop(stdin);
        child.wait().await
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Windows desktop handoff timed out.",
        )
    })?
}

fn windows_tool() -> Option<PathBuf> {
    let windows = PathBuf::from("/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe");
    (windows.is_file()
        && std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|release| release.to_ascii_lowercase().contains("wsl")))
    .then_some(windows)
}
