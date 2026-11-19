use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use reviewq_app::access::AppError;
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::settings::CheckoutBinding;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const OUTPUT_LIMIT: u64 = 64 * 1024;

/// Runs Git using a trusted static argument list against one explicit checkout.
pub async fn git_output(directory: &Path, arguments: &[&str]) -> Result<Vec<u8>, AppError> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_CONFIG",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
        ] {
            command.env_remove(key);
        }
        let mut child = command.spawn().map_err(|_| AppError::Unavailable)?;
        let mut body = Vec::new();
        child
            .stdout
            .take()
            .ok_or(AppError::Unavailable)?
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut body)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if body.len() as u64 > OUTPUT_LIMIT {
            return Err(AppError::PayloadTooLarge);
        }
        if !child
            .wait()
            .await
            .map_err(|_| AppError::Unavailable)?
            .success()
        {
            return Err(AppError::NotFound);
        }
        Ok(body)
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

pub(crate) async fn verify_checkout(
    path: &AbsolutePath,
) -> Result<(CheckoutBinding, String), AppError> {
    let selected = tokio::fs::canonicalize(path.as_path())
        .await
        .map_err(|_| AppError::NotFound)?;
    let root = resolve(&selected, "--show-toplevel").await?;
    if root.as_path() != selected {
        return Err(AppError::InvalidRequest);
    }
    let common_dir = resolve(&selected, "--git-common-dir").await?;
    let remote = git_output(
        &selected,
        &["config", "--local", "--get", "remote.origin.url"],
    )
    .await?;
    let remote = std::str::from_utf8(&remote)
        .map_err(|_| AppError::InvalidRequest)?
        .trim_end_matches('\n');
    if remote.is_empty() || remote.len() > 4096 || remote.chars().any(char::is_control) {
        return Err(AppError::InvalidRequest);
    }
    Ok((CheckoutBinding { root, common_dir }, remote.to_owned()))
}

async fn resolve(directory: &Path, target: &str) -> Result<AbsolutePath, AppError> {
    let output = git_output(directory, &["rev-parse", "--path-format=absolute", target]).await?;
    let text = std::str::from_utf8(&output).map_err(|_| AppError::InvalidRequest)?;
    let path = PathBuf::from(text.strip_suffix('\n').unwrap_or(text));
    if !path.is_absolute() {
        return Err(AppError::InvalidRequest);
    }
    let path = tokio::fs::canonicalize(path)
        .await
        .map_err(|_| AppError::NotFound)?;
    if !path.is_dir() {
        return Err(AppError::InvalidRequest);
    }
    AbsolutePath::from_canonical(path).map_err(|_| AppError::InvalidRequest)
}
