use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use reviewq_transport::ServiceIdentity;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::install::{self, Failure, paths::Paths};
use crate::{cli::Open, native_client, runtime, runtime_settings};

const UNIT: &str = "reviewq.service";
/// The start door's socket unit. Each connection to it starts one `reviewq start-door`.
pub(crate) const START_SOCKET: &str = "reviewq-start.socket";
/// An instance of the start door's per-connection template, named only to ask the user
/// manager which file the template loads from.
const START_DOOR_INSTANCE: &str = "reviewq-start@check.service";
/// Every running start door connection.
pub(crate) const START_DOOR_INSTANCES: &str = "reviewq-start@*.service";
/// The transient service the dashboard's uninstall runs in, outside `reviewq.service`.
pub(crate) const UNINSTALL_JOB: &str = "reviewq-uninstall";
const CLAUDE_DEFAULT: &str = "UnsetEnvironment=CLAUDE_CONFIG_DIR";

pub(crate) fn unit(
    paths: &Paths,
    template: &Path,
    claude_config_dir: Option<&Path>,
) -> Result<Vec<u8>, Failure> {
    let mut text = std::fs::read_to_string(template)?;
    replace_once(&mut text, "@executable@", &executable(paths)?)?;
    for (placeholder, key, value) in [
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
    ] {
        replace_once(&mut text, placeholder, &unit_arg(key, &value)?)?;
    }
    // Claude stores default project MCP settings directly under HOME.
    let claude = match claude_config_dir {
        Some(path) => format!("Environment={}", unit_arg("CLAUDE_CONFIG_DIR=", path)?),
        None => CLAUDE_DEFAULT.to_owned(),
    };
    replace_once(&mut text, "Environment=@claude@", &claude)?;
    Ok(text.into_bytes())
}

pub(crate) fn unit_has_claude_override(unit: &[u8]) -> bool {
    !unit
        .split(|byte| *byte == b'\n')
        .any(|line| line == CLAUDE_DEFAULT.as_bytes())
}

/// The installed launcher's executable as a quoted unit argument, for every `ExecStart`.
pub(crate) fn executable(paths: &Paths) -> Result<String, Failure> {
    unit_arg("", &paths.profile.join("current/bin/reviewq"))
}

fn unit_arg(key: &str, path: &Path) -> Result<String, Failure> {
    let value = path
        .to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or("A service path must be UTF-8 without control characters.")?;
    Ok(format!(
        "\"{key}{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    ))
}

fn replace_once(text: &mut String, placeholder: &str, value: &str) -> Result<(), Failure> {
    if text.matches(placeholder).count() != 1 {
        return Err(format!("Invalid service template placeholder {placeholder}.").into());
    }
    *text = text.replace(placeholder, value);
    Ok(())
}

pub(crate) async fn manager() -> Result<(), Failure> {
    systemctl(&["show", "--property=Version", "--value"], false).await?;
    Ok(())
}

/// Fails when the user manager loads ReviewQ's service, start socket or start door template
/// from a file other than the one ReviewQ installs. An instance reports its template's file.
pub(crate) async fn unit_location(paths: &Paths) -> Result<(), Failure> {
    for (unit, path) in [
        (UNIT, &paths.unit),
        (START_SOCKET, &paths.start_socket),
        (START_DOOR_INSTANCE, &paths.start_door),
    ] {
        let (success, location) =
            systemctl(&["show", unit, "--property=FragmentPath", "--value"], true).await?;
        if success && !location.is_empty() && Path::new(&location) != path {
            return Err(install::refused(format!(
                "Preserved the unrelated unit at {location}"
            )));
        }
    }
    Ok(())
}

/// Whether `reviewq.service` starts at login, and whether it is running now.
pub(crate) async fn state() -> Result<(bool, bool), Failure> {
    unit_state(UNIT).await
}

/// Whether `unit` is enabled, and whether it is active or on its way there.
pub(crate) async fn unit_state(unit: &str) -> Result<(bool, bool), Failure> {
    let (_, enabled) = systemctl(&["is-enabled", unit], true).await?;
    let (_, active) = systemctl(&["is-active", unit], true).await?;
    Ok((
        enabled == "enabled",
        matches!(active.as_str(), "active" | "activating" | "reloading"),
    ))
}

/// Whether `unit` is in the failed state, as after its start limit was hit.
pub(crate) async fn failed(unit: &str) -> Result<bool, Failure> {
    Ok(systemctl(&["is-failed", unit], true).await?.1 == "failed")
}

pub(crate) async fn reload() -> Result<(), Failure> {
    systemctl(&["daemon-reload"], false).await?;
    Ok(())
}

pub(crate) async fn switch(verb: &str) -> Result<(), Failure> {
    control(verb, UNIT).await
}

/// Runs `systemctl --user <verb> <unit>` and fails when it does.
pub(crate) async fn control(verb: &str, unit: &str) -> Result<(), Failure> {
    systemctl(&[verb, unit], false).await?;
    Ok(())
}

/// Asks the user manager to start `reviewq.service` and returns without waiting. A start
/// asked for while a stop runs is queued after it.
pub(crate) async fn start_without_waiting() -> Result<(), Failure> {
    systemctl(&["--no-block", "start", UNIT], false).await?;
    Ok(())
}

/// Asks the user manager to stop `reviewq.service` and returns without waiting for the stop.
pub(crate) async fn stop_without_waiting() -> Result<(), Failure> {
    systemctl(&["--no-block", "stop", UNIT], false).await?;
    Ok(())
}

pub(crate) enum StartupCheck {
    Identity,
    Recovered,
    Operational,
}

pub(crate) async fn ready(
    paths: &Paths,
    digest: &str,
    schema: i64,
    check: StartupCheck,
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
                let ready = native_client::verify_service(&endpoint, &bytes)?;
                let identity = endpoint.identity().clone();
                if identity.store_schema != schema || identity.store_application_id != 0x5256_5132 {
                    return Err("The service opened an unexpected store format.".into());
                }
                let confirmed = match check {
                    StartupCheck::Identity => true,
                    StartupCheck::Recovered => ready.recovery_complete,
                    StartupCheck::Operational => ready.ready,
                };
                if confirmed {
                    return Ok(identity);
                }
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
    if let Err(error) = install::start_door::repair(paths).await {
        eprintln!("ReviewQ's Start button may not work: {error}");
    }
    if !state().await?.1 {
        switch("start").await?;
    }
    ready(
        paths,
        &record.binary,
        record.store,
        StartupCheck::Operational,
    )
    .await?;
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
    install::verify_unit(&paths)?;
    unit_location(&paths).await?;
    let (_, text) = capture(
        "journalctl",
        // Without `--user`, every journal this user can read is searched, so the units'
        // entries are found where the user manager logs to the system journal as well. The
        // start door's connections are included, so a failed start from the stopped page shows.
        &[
            "--user-unit",
            UNIT,
            "--user-unit",
            "reviewq-start@*",
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
        let identity = ready(
            &paths,
            &release.binary,
            release.store,
            StartupCheck::Operational,
        )
        .await?;
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
