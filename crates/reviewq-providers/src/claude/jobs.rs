use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use reviewq_app::access::AppError;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Job {
    pub(super) session_id: Option<String>,
    cwd: PathBuf,
    pub(crate) kind: JobKind,
    pub(crate) id: Option<String>,
    pub(crate) pid: Option<u32>,
    pub(super) name: Option<String>,
    pub(super) started_at: Option<i64>,
    pub(super) state: Option<String>,
    pub(super) status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum JobKind {
    Interactive,
    Background,
    #[serde(other)]
    Unknown,
}

pub(crate) fn native_command(executable: &Path, config_dir: Option<&Path>, cwd: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env_clear()
        .current_dir(cwd)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = config_dir {
        command.env("CLAUDE_CONFIG_DIR", path);
    }
    for key in [
        "HOME",
        "PATH",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "COLORTERM",
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "SSH_AUTH_SOCK",
        "WSL_DISTRO_NAME",
        "WSL_INTEROP",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_EXTRA_CA_CERTS",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

pub(crate) async fn claude_job(
    command: Command,
    cwd: &Path,
    session_id: &str,
) -> Result<Option<Job>, AppError> {
    let rows = jobs(command, cwd).await?;
    select(
        rows,
        cwd,
        |job| job.session_id.as_deref() == Some(session_id),
        false,
    )
}

/// Finds the unique background job for a known launch address or its reserved name.
pub(super) async fn claude_launch_job(
    command: Command,
    cwd: &Path,
    job_id: Option<&str>,
    session_id: Option<&str>,
    name: Option<&str>,
) -> Result<Option<Job>, AppError> {
    let rows = jobs(command, cwd).await?;
    let job = select(
        rows,
        cwd,
        |job| {
            if let Some(id) = job_id {
                job.id.as_deref() == Some(id)
            } else if let Some(id) = session_id {
                job.session_id.as_deref() == Some(id)
            } else {
                name.is_some_and(|name| job.name.as_deref() == Some(name))
            }
        },
        true,
    )?;
    if job.as_ref().is_some_and(|job| {
        !matches!(job.kind, JobKind::Background)
            || session_id.is_some_and(|id| job.session_id.as_deref() != Some(id))
            || (session_id.is_none() && name.is_some_and(|name| job.name.as_deref() != Some(name)))
    }) {
        return Err(AppError::Conflict);
    }
    Ok(job)
}

async fn jobs(mut command: Command, cwd: &Path) -> Result<Vec<Job>, AppError> {
    let mut child = command
        .args(["agents", "--json", "--all", "--cwd"])
        .arg(cwd)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|_| AppError::Unavailable)?;
    let output = child.stdout.take().ok_or(AppError::Unavailable)?;
    tokio::time::timeout(Duration::from_secs(4), async move {
        let mut body = Vec::new();
        output
            .take(256 * 1024 + 1)
            .read_to_end(&mut body)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if body.len() > 256 * 1024 {
            return Err(AppError::PayloadTooLarge);
        }
        if !child.wait().await.is_ok_and(|value| value.success()) {
            return Err(AppError::Unavailable);
        }
        serde_json::from_slice::<Vec<Job>>(&body).map_err(|_| AppError::Unavailable)
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

fn select(
    mut rows: Vec<Job>,
    cwd: &Path,
    matches: impl Fn(&Job) -> bool,
    verify_aliases: bool,
) -> Result<Option<Job>, AppError> {
    let Some(index) = rows.iter().position(&matches) else {
        return Ok(None);
    };
    let job = rows.swap_remove(index);
    if rows.iter().any(|other| {
        matches(other)
            || (verify_aliases
                && (job.id.is_some() && job.id == other.id
                    || job.session_id.is_some() && job.session_id == other.session_id))
    }) || job.cwd != cwd
        || job.id.as_ref().is_some_and(|id| {
            id.is_empty() || id.len() > 64 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        return Err(AppError::Conflict);
    }
    if matches!(job.kind, JobKind::Unknown | JobKind::Background)
        && job.pid.is_some()
        && job.id.is_none()
    {
        return Err(AppError::Unavailable);
    }
    Ok(Some(job))
}
