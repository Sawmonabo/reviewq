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
    session_id: Option<String>,
    cwd: PathBuf,
    pub(crate) kind: JobKind,
    pub(crate) id: Option<String>,
    pub(crate) pid: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum JobKind {
    Interactive,
    Background,
    #[serde(other)]
    Unknown,
}

pub(crate) async fn claude_job(
    executable: &Path,
    profile: &Path,
    cwd: &Path,
    session_id: &str,
) -> Result<Option<Job>, AppError> {
    let mut child = Command::new(executable)
        .args(["agents", "--json", "--all", "--cwd"])
        .arg(cwd)
        .current_dir(cwd)
        .env("CLAUDE_CONFIG_DIR", profile)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| AppError::Unavailable)?;
    let output = child.stdout.take().ok_or(AppError::Unavailable)?;
    let rows = tokio::time::timeout(Duration::from_secs(4), async move {
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
    .map_err(|_| AppError::Unavailable)??;
    let mut matches = rows
        .into_iter()
        .filter(|job| job.session_id.as_deref() == Some(session_id));
    let Some(job) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some()
        || job.cwd != cwd
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
