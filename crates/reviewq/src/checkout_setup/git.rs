use std::ffi::OsStr;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use reviewq_app::access::AppError;
use reviewq_providers::git_output;

use super::Failure;

pub(super) const MAX_WORKTREES: usize = 64;

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
    let mut members = Vec::with_capacity(worktrees.len());
    for root in worktrees {
        if belongs(&root, &common).await? {
            members.push(root);
        } else if root == primary || root == worktree {
            return Err("A linked worktree does not match this Git repository.".into());
        }
        // Git keeps a linked worktree's record when its folder was deleted and something else,
        // such as another repository, now stands there. That folder is not this checkout's.
    }
    let mut materialized = Vec::with_capacity(members.len());
    for root in members {
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
    if tracked(root, paths).await? {
        return Err(format!(
            "Checkout setup would touch a tracked destination under {}.",
            root.display()
        )
        .into());
    }
    Ok(())
}

async fn tracked(root: &Path, paths: &[&str]) -> Result<bool, Failure> {
    let mut args = vec!["--literal-pathspecs", "ls-files", "-z", "--"];
    args.extend(paths);
    Ok(!git_output(root, &args).await?.is_empty())
}

/// Whether `root` is still a worktree of the repository at `common`, its top level. It is not
/// when Git says the folder is no repository or names another; when Git cannot answer, such as
/// for a timeout, that is an error.
pub(super) async fn belongs(root: &Path, common: &Path) -> Result<bool, Failure> {
    for (name, expected) in [("--git-common-dir", common), ("--show-toplevel", root)] {
        match repo_path(root, name).await {
            Ok(found) if found == expected => (),
            Ok(_) => return Ok(false),
            Err(error) if matches!(error.downcast_ref(), Some(AppError::NotFound)) => {
                return Ok(false);
            }
            Err(_) => {
                return Err(format!(
                    "Git could not say whether {} is still a worktree of this repository",
                    root.display()
                )
                .into());
            }
        }
    }
    Ok(true)
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

/// Where a file checkout setup wrote at `relative` in one of the checkout's worktrees stands.
pub(super) enum Destination {
    /// Its worktree is still the checkout's and Git does not track it.
    Owned,
    /// Its worktree is still the checkout's, but Git now tracks it.
    Tracked,
    /// Its folder is no longer a worktree of the checkout, such as a removed worktree whose
    /// folder now holds another repository.
    Elsewhere,
}

pub(super) async fn destination(
    file: &Path,
    common: &Path,
    relative: &str,
) -> Result<Destination, Failure> {
    let count = Path::new(relative).components().count();
    // A record naming no such file in a worktree is damaged, so nothing there is ReviewQ's.
    let Some(worktree) = file
        .ancestors()
        .nth(count)
        .filter(|worktree| worktree.join(relative) == file)
    else {
        return Ok(Destination::Elsewhere);
    };
    Ok(if !belongs(worktree, common).await? {
        Destination::Elsewhere
    } else if tracked(worktree, &[relative])
        .await
        .map_err(|_| format!("Git could not say whether it tracks {}", file.display()))?
    {
        Destination::Tracked
    } else {
        Destination::Owned
    })
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
