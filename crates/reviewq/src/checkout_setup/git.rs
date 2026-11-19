use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::Failure;

const GIT_OUTPUT: u64 = 64 * 1024;

pub(super) struct Checkout {
    pub worktree: PathBuf,
    pub common: PathBuf,
    pub primary: PathBuf,
}

pub(super) async fn discover(directory: Option<&Path>) -> Result<Checkout, Failure> {
    let directory = directory
        .map(Path::to_owned)
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    let worktree = path(
        &git(
            &directory,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        )
        .await?,
    )?;
    let common = path(
        &git(
            &worktree,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await?,
    )?;
    let locations = git(&worktree, &["worktree", "list", "--porcelain", "-z"]).await?;
    let primary = locations
        .split(|byte| *byte == 0)
        .next()
        .and_then(|line| line.strip_prefix(b"worktree "))
        .ok_or("Could not verify the main Git worktree.")?;
    let primary = path(primary)?;
    let source = git(
        &worktree,
        &["config", "--local", "--get", "remote.origin.url"],
    )
    .await?;
    let remote = std::str::from_utf8(&source)?.trim_end_matches('\n');
    if remote.len() > 4096
        || remote.chars().any(char::is_control)
        || remote.contains('?')
        || remote.contains('#')
        || reqwest::Url::parse(remote).ok().is_some_and(|url| {
            url.password().is_some()
                || (matches!(url.scheme(), "https" | "http") && !url.username().is_empty())
        })
    {
        return Err("The Git origin must identify the repository without credentials.".into());
    }
    let managed = [
        ".codex/config.toml",
        ".agents/skills/reviewq-manage/SKILL.md",
        ".claude/skills/reviewq-manage/SKILL.md",
    ];
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend(managed);
    if !git(&worktree, &args).await?.is_empty()
        || (primary != worktree && !git(&primary, &args).await?.is_empty())
    {
        return Err("Checkout setup would touch a tracked ReviewQ destination.".into());
    }
    Ok(Checkout {
        worktree,
        common,
        primary,
    })
}

pub(super) async fn common_directory(directory: Option<&Path>) -> Result<PathBuf, Failure> {
    let directory = directory
        .map(Path::to_owned)
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    path(
        &git(
            &directory,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await?,
    )
}

fn path(value: &[u8]) -> Result<PathBuf, Failure> {
    let path = Path::new(OsStr::from_bytes(
        value.strip_suffix(b"\n").unwrap_or(value),
    ));
    if !path.is_absolute() || path.to_str().is_none() {
        return Err("ReviewQ needs an absolute UTF-8 Git directory.".into());
    }
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err("Git did not return a directory.".into());
    }
    Ok(path)
}

async fn git(directory: &Path, args: &[&str]) -> Result<Vec<u8>, Failure> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .ok_or("Git returned no output stream.")?
            .take(GIT_OUTPUT + 1)
            .read_to_end(&mut output)
            .await?;
        if output.len() as u64 > GIT_OUTPUT || !child.wait().await?.success() {
            return Err("Could not verify the selected Git checkout.".into());
        }
        Ok(output)
    })
    .await
    .map_err(|_| "Git checkout discovery timed out.")?
}
