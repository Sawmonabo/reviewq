use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use reviewq_transport::ServiceIdentity;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::install::{self, Failure, paths::Paths};
use crate::{cli::Open, native_client, runtime, runtime_settings};

const UNIT: &str = "reviewq.service";

pub(crate) fn unit(paths: &Paths, template: &Path) -> Result<Vec<u8>, Failure> {
    let mut text = std::fs::read_to_string(template)?;
    for (placeholder, key, value) in [
        (
            "@executable@",
            "",
            paths.profile.join("current/bin/reviewq"),
        ),
        ("@home@", "HOME=", paths.home.clone()),
        (
            "@data@",
            "XDG_DATA_HOME=",
            paths
                .profile
                .parent()
                .ok_or("Missing data root.")?
                .to_path_buf(),
        ),
        ("@config@", "XDG_CONFIG_HOME=", paths.config_home.clone()),
        ("@state@", "XDG_STATE_HOME=", paths.state_home.clone()),
        ("@codex@", "CODEX_HOME=", paths.codex_home.clone()),
        ("@claude@", "CLAUDE_CONFIG_DIR=", paths.claude_home.clone()),
    ] {
        let value = value
            .to_str()
            .filter(|text| !text.chars().any(char::is_control))
            .ok_or("A service path must be UTF-8 without control characters.")?;
        let arg = format!(
            "\"{key}{}\"",
            value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('%', "%%")
        );
        if text.matches(placeholder).count() != 1 {
            return Err(format!("Invalid service template placeholder {placeholder}.").into());
        }
        text = text.replace(placeholder, &arg);
    }
    Ok(text.into_bytes())
}

pub(crate) async fn manager() -> Result<(), Failure> {
    systemctl(&["show", "--property=Version", "--value"], false).await?;
    Ok(())
}

pub(crate) async fn unit_location(paths: &Paths) -> Result<(), Failure> {
    let (success, location) =
        systemctl(&["show", UNIT, "--property=FragmentPath", "--value"], true).await?;
    if success && !location.is_empty() && Path::new(&location) != paths.unit {
        return Err(format!("Preserved the unrelated service at {location}").into());
    }
    Ok(())
}

pub(crate) async fn state() -> Result<(bool, bool), Failure> {
    let (_, enabled) = systemctl(&["is-enabled", UNIT], true).await?;
    let (_, active) = systemctl(&["is-active", UNIT], true).await?;
    Ok((
        enabled == "enabled",
        matches!(active.as_str(), "active" | "activating" | "reloading"),
    ))
}

pub(crate) async fn reload() -> Result<(), Failure> {
    systemctl(&["daemon-reload"], false).await?;
    Ok(())
}

pub(crate) async fn switch(verb: &str) -> Result<(), Failure> {
    systemctl(&[verb, UNIT], false).await?;
    Ok(())
}

pub(crate) async fn ready(
    paths: &Paths,
    digest: &str,
    schema: i64,
) -> Result<ServiceIdentity, Failure> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let executable = paths.profile.join("current/bin/reviewq").canonicalize()?;
    let client = native_client::client()?;
    loop {
        let (found, pid) =
            systemctl(&["show", UNIT, "--property=MainPID", "--value"], true).await?;
        if found
            && let Ok(pid) = pid.parse::<u32>()
            && pid != 0
            && std::fs::read_link(format!("/proc/{pid}/exe"))
                .is_ok_and(|running| running == executable)
            && let Ok(endpoint) = runtime::read_endpoint_with_build(&paths.profile, digest)
        {
            let status = native_client::signed(&client, &endpoint, "GET", "/api/v1/status", b"");
            if let Ok(Ok(bytes)) = tokio::time::timeout_at(deadline, status).await {
                native_client::verify_ready(&endpoint, &bytes)?;
                let identity = endpoint.identity().clone();
                if identity.store_schema != schema || identity.store_application_id != 0x5256_5132 {
                    return Err("The service opened an unexpected store format.".into());
                }
                return Ok(identity);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(
                "The verified ReviewQ service did not become ready. Inspect reviewq logs.".into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
}

pub(crate) async fn operate(start: bool) -> Result<(), Failure> {
    let paths = Paths::resolve()?;
    let _lock = paths.lock()?;
    let record = install::active(&paths)?;
    manager().await?;
    unit_location(&paths).await?;
    if start {
        ensure_running(&paths, &record).await?;
        println!("ReviewQ is running.");
    } else {
        switch("stop").await?;
        println!("ReviewQ is stopped.");
    }
    Ok(())
}

async fn ensure_running(paths: &Paths, record: &install::Release) -> Result<(), Failure> {
    runtime_settings::Settings::load(&paths.profile, false)?;
    if !state().await?.1 {
        switch("start").await?;
    }
    ready(paths, &record.binary, record.store).await?;
    Ok(())
}

pub(crate) async fn open(
    profile: Option<std::path::PathBuf>,
    options: Open,
) -> Result<(), Failure> {
    let installed_lookup = profile.is_none();
    let profile = runtime::profile(profile)?;
    if installed_lookup {
        let paths = Paths::resolve()?;
        if let Some(record) = install::installed(&paths)? {
            let _lock = paths.lock()?;
            let current = install::active(&paths)?;
            if current != record {
                return Err("The active installation changed. Retry opening ReviewQ.".into());
            }
            manager().await?;
            unit_location(&paths).await?;
            ensure_running(&paths, &current).await?;
        }
    }
    native_client::open(&profile, options).await
}

pub(crate) async fn logs() -> Result<(), Failure> {
    let paths = Paths::resolve()?;
    install::active(&paths)?;
    let (_, text) = capture(
        "journalctl",
        &[
            "--user",
            "--unit",
            UNIT,
            "--lines",
            "100",
            "--no-pager",
            "--output",
            "cat",
        ],
        false,
    )
    .await?;
    println!("{text}");
    Ok(())
}

pub(crate) async fn notify() -> Result<(), Failure> {
    let paths = Paths::resolve()?;
    if let Some(release) = install::installed(&paths)?
        && let Some(windows) = release.windows_sender()?
    {
        let identity = ready(&paths, &release.binary, release.store).await?;
        let endpoint = runtime::read_endpoint_with_build(&paths.profile, &identity.build_digest)?;
        match windows.test(endpoint.browser()).await {
            reviewq_app::notifications::DesktopOutcome::Delivered => {
                println!("The Windows desktop notification service accepted the test.");
                return Ok(());
            }
            reviewq_app::notifications::DesktopOutcome::NotStarted(_) => {
                return Err("The Windows desktop notification service is unavailable.".into());
            }
            reviewq_app::notifications::DesktopOutcome::Uncertain(_) => {
                return Err("Windows could not confirm the desktop test notification.".into());
            }
        }
    }
    let binary = runtime_settings::executable("notify-send")
        .ok_or("The Linux desktop notification client is unavailable.")?;
    capture(
        binary
            .to_str()
            .ok_or("The notification client path is not UTF-8.")?,
        &[
            "--app-name=ReviewQ",
            "--",
            "ReviewQ test notification",
            "ReviewQ can reach your desktop.",
        ],
        false,
    )
    .await?;
    println!("The desktop notification service accepted the test.");
    Ok(())
}

async fn systemctl(args: &[&str], allow_failure: bool) -> Result<(bool, String), Failure> {
    let mut flags = vec!["--user", "--no-pager"];
    flags.extend_from_slice(args);
    capture("systemctl", &flags, allow_failure).await
}

pub(crate) async fn capture(
    program: &str,
    args: &[&str],
    allow_failure: bool,
) -> Result<(bool, String), Failure> {
    let mut command = Command::new(program);
    command.args(args);
    output(command, allow_failure).await
}

pub(crate) async fn output(
    mut command: Command,
    allow_failure: bool,
) -> Result<(bool, String), Failure> {
    let program = command.as_std().get_program().to_os_string();
    tokio::time::timeout(Duration::from_secs(25), async {
        let mut child = command
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut text = Vec::new();
        child
            .stdout
            .take()
            .ok_or("Output not available.")?
            .take(512 * 1024 + 1)
            .read_to_end(&mut text)
            .await?;
        if text.len() > 512 * 1024 {
            return Err("Command output exceeded the 512 KiB limit.".into());
        }
        let success = child.wait().await?.success();
        if !success && !allow_failure {
            let command = Path::new(&program)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            return Err(format!("{command} did not complete successfully.").into());
        }
        Ok((success, String::from_utf8(text)?.trim().to_owned()))
    })
    .await
    .map_err(|_| "A local lifecycle command timed out.")?
}
