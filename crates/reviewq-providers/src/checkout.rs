use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use reviewq_app::access::AppError;
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::settings::CheckoutBinding;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const OUTPUT_LIMIT: u64 = 64 * 1024;
const MESSAGE_LIMIT: usize = 512;

/// Git ran and exited with a failure: its exit code and what it wrote to standard error, as one
/// line, such as "fatal: bad config line 6 in file .git/config".
#[derive(Debug)]
pub struct GitRefusal {
    pub code: Option<i32>,
    pub message: String,
}

impl std::fmt::Display for GitRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.message.is_empty(), self.code) {
            (false, _) => formatter.write_str(&self.message),
            (true, Some(code)) => write!(formatter, "Git exited with status {code}"),
            (true, None) => formatter.write_str("Git was stopped by a signal"),
        }
    }
}

impl std::error::Error for GitRefusal {}

/// Runs Git using a trusted static argument list against one explicit checkout. Any failure
/// is `NotFound`; `git_run` tells Git's refusals apart.
pub async fn git_output(directory: &Path, arguments: &[&str]) -> Result<Vec<u8>, AppError> {
    git_run(directory, arguments)
        .await?
        .map_err(|_| AppError::NotFound)
}

/// Runs Git as `git_output` does. The outer error is Git not answering, such as a timeout; the
/// inner one is Git's own refusal, with its exit code and words.
pub async fn git_run(
    directory: &Path,
    arguments: &[&str],
) -> Result<Result<Vec<u8>, GitRefusal>, AppError> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let mut stdout = child
            .stdout
            .take()
            .ok_or(AppError::Unavailable)?
            .take(OUTPUT_LIMIT + 1);
        let mut stderr = child
            .stderr
            .take()
            .ok_or(AppError::Unavailable)?
            .take(MESSAGE_LIMIT as u64 * 4);
        let (mut body, mut said) = (Vec::new(), Vec::new());
        // Both are read at once, so a full pipe on either side cannot stall Git.
        let (read, _) = tokio::join!(stdout.read_to_end(&mut body), stderr.read_to_end(&mut said));
        drop((stdout, stderr));
        read.map_err(|_| AppError::Unavailable)?;
        if body.len() as u64 > OUTPUT_LIMIT {
            return Err(AppError::PayloadTooLarge);
        }
        let status = child.wait().await.map_err(|_| AppError::Unavailable)?;
        if !status.success() {
            return Ok(Err(GitRefusal {
                code: status.code(),
                message: one_line(&said),
            }));
        }
        Ok(Ok(body))
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

/// What Git wrote as one line of at most `MESSAGE_LIMIT` bytes, so it reads inside a sentence.
fn one_line(said: &[u8]) -> String {
    let text = String::from_utf8_lossy(said);
    let mut line = text
        .lines()
        .map(|line| line.trim().trim_end_matches('.'))
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.chars()
                .filter(|ch| !ch.is_control())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("; ");
    if line.len() > MESSAGE_LIMIT {
        let cut = (0..=MESSAGE_LIMIT - 3)
            .rev()
            .find(|at| line.is_char_boundary(*at))
            .unwrap_or(0);
        line.truncate(cut);
        line.push('…');
    }
    line
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
