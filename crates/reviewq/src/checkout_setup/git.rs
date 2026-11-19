use std::ffi::OsStr;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use reviewq_providers::git_output;

use super::Failure;

const MAX_WORKTREES: usize = 64;

pub(super) struct Checkout {
    pub root: PathBuf,
    pub common: PathBuf,
    pub primary: PathBuf,
    pub worktrees: Vec<PathBuf>,
}

pub(super) async fn discover(directory: Option<&Path>) -> Result<Checkout, Failure> {
    let directory = selected(directory)?;
    let worktree = repo_path(&directory, "--show-toplevel").await?;
    let common = repo_path(&worktree, "--git-common-dir").await?;
    let locations = git_output(&worktree, &["worktree", "list", "--porcelain", "-z"]).await?;
    let mut entries = locations
        .split(|byte| *byte == 0)
        .filter_map(|line| line.strip_prefix(b"worktree "));
    let primary = path(entries.next().ok_or("Missing main Git worktree.")?)?;
    let mut worktrees = vec![primary.clone()];
    for value in entries {
        let location = raw_path(value)?;
        match location.try_exists() {
            Ok(true) => worktrees.push(path(value)?),
            Ok(false) => continue,
            Err(error) => return Err(error.into()),
        }
        if worktrees.len() > MAX_WORKTREES {
            return Err("The repository exceeds the supported number of linked worktrees.".into());
        }
    }
    if !worktrees.contains(&worktree) {
        return Err("The active checkout does not belong to the recorded Git worktrees.".into());
    }
    for root in &worktrees {
        if repo_path(root, "--git-common-dir").await? != common
            || repo_path(root, "--show-toplevel").await? != *root
        {
            return Err("A linked worktree does not match this Git repository.".into());
        }
    }
    let mut materialized = Vec::with_capacity(worktrees.len());
    for root in worktrees {
        let index = git_output(
            &root,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )
        .await?;
        let index = raw_path(index.strip_suffix(b"\n").unwrap_or(&index))?;
        if index.try_exists()? {
            materialized.push(root);
        }
    }
    Ok(Checkout {
        root: worktree,
        common,
        primary,
        worktrees: materialized,
    })
}

pub(super) async fn common_directory(directory: Option<&Path>) -> Result<PathBuf, Failure> {
    repo_path(&selected(directory)?, "--git-common-dir").await
}

pub(super) async fn current_worktree() -> Result<PathBuf, Failure> {
    repo_path(&selected(None)?, "--show-toplevel").await
}

pub(super) async fn require_untracked(root: &Path, paths: &[&str]) -> Result<(), Failure> {
    let mut args = vec!["--literal-pathspecs", "ls-files", "-z", "--"];
    args.extend(paths);
    if !git_output(root, &args).await?.is_empty() {
        return Err(format!(
            "Checkout setup would touch a tracked destination under {}.",
            root.display()
        )
        .into());
    }
    Ok(())
}

pub(super) async fn require_ignored(root: &Path, paths: &[&str]) -> Result<(), Failure> {
    for path in paths {
        if git_output(root, &["check-ignore", "--no-index", "--quiet", "--", path])
            .await
            .is_err()
        {
            return Err(format!(
                "Checkout setup requires Git to ignore {}.",
                root.join(path).display()
            )
            .into());
        }
    }
    Ok(())
}

pub(super) async fn private_config(file: &Path, needs_ignore: bool) -> Result<(), Failure> {
    let parent = file.parent().ok_or("Missing Claude profile directory.")?;
    for ancestor in parent.ancestors() {
        match ancestor.join(".git").symlink_metadata() {
            Ok(_) => {
                let root = repo_path(ancestor, "--show-toplevel").await?;
                let relative = file
                    .strip_prefix(&root)?
                    .to_str()
                    .ok_or("Expected a UTF-8 Claude profile path.")?;
                require_untracked(&root, &[relative]).await?;
                if needs_ignore {
                    return require_ignored(&root, &[relative]).await;
                }
                return Ok(());
            }
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) async fn verify_removal(
    file: &Path,
    common: &Path,
    relative: &str,
) -> Result<(), Failure> {
    let count = Path::new(relative).components().count();
    let worktree = file
        .ancestors()
        .nth(count)
        .ok_or("Invalid owned file address.")?;
    if worktree.join(relative) != file
        || repo_path(worktree, "--git-common-dir").await? != common
        || repo_path(worktree, "--show-toplevel").await? != worktree
    {
        return Err("The owned destination no longer belongs to the configured checkout.".into());
    }
    require_untracked(worktree, &[relative]).await
}

fn selected(directory: Option<&Path>) -> Result<PathBuf, Failure> {
    Ok(directory
        .map(Path::to_owned)
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?)
}

async fn repo_path(directory: &Path, name: &str) -> Result<PathBuf, Failure> {
    let output = git_output(directory, &["rev-parse", "--path-format=absolute", name]).await?;
    path(output.strip_suffix(b"\n").unwrap_or(&output))
}

fn raw_path(value: &[u8]) -> Result<&Path, Failure> {
    let path = Path::new(OsStr::from_bytes(value));
    if !path.is_absolute() || path.to_str().is_none() {
        return Err("ReviewQ needs an absolute UTF-8 Git directory.".into());
    }
    Ok(path)
}

fn path(value: &[u8]) -> Result<PathBuf, Failure> {
    let path = raw_path(value)?.canonicalize()?;
    if !path.is_dir() {
        return Err("Git did not return a directory.".into());
    }
    Ok(path)
}
