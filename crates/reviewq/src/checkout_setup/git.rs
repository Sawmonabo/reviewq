use std::ffi::OsStr;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use reviewq_app::access::AppError;
use reviewq_providers::git_run;

use super::Failure;

pub(super) const MAX_WORKTREES: usize = 64;

pub(super) struct Checkout {
    pub root: PathBuf,
    pub common: PathBuf,
    pub primary: PathBuf,
    pub worktrees: Vec<PathBuf>,
}

/// Git could not read `folder`: `cause` is Git's own words, or why it gave none.
#[derive(Debug)]
pub(super) struct Unread {
    pub(super) folder: PathBuf,
    pub(super) cause: String,
}

impl std::fmt::Display for Unread {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Git could not read {}: {}",
            self.folder.display(),
            self.cause
        )
    }
}

impl std::error::Error for Unread {}

pub(super) async fn discover(directory: Option<&Path>) -> Result<Checkout, Failure> {
    let directory = selected(directory)?;
    let worktree = repo_path(&directory, "--show-toplevel").await?;
    let common = repo_path(&worktree, "--git-common-dir").await?;
    let locations = answer(&worktree, &["worktree", "list", "--porcelain", "-z"]).await?;
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
        let index = answer(
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
    let unsure = |cause: &dyn std::fmt::Display| {
        format!(
            "Git could not say whether it tracks {} in {}: {cause}",
            paths.join(" or "),
            root.display()
        )
    };
    match git_run(root, &args).await {
        Ok(Ok(listed)) => Ok(!listed.is_empty()),
        Ok(Err(refusal)) => Err(unsure(&refusal).into()),
        Err(error) => Err(unsure(&unanswered(&error)).into()),
    }
}

/// Whether `root` is still a worktree of the repository at `common`, its top level. It is not
/// when Git names another, or when `no_repository` holds for the folder. Any other failure,
/// such as a timeout or a bad line in a Git config, is an error with Git's words.
pub(super) async fn belongs(root: &Path, common: &Path) -> Result<bool, Failure> {
    for (name, expected) in [("--git-common-dir", common), ("--show-toplevel", root)] {
        let unsure = |cause: &dyn std::fmt::Display| {
            format!(
                "Git could not say whether {} is still a worktree of this repository: {cause}",
                root.display()
            )
        };
        let output = git_run(root, &["rev-parse", "--path-format=absolute", name]).await;
        match output {
            Ok(Ok(output)) => match path(output.strip_suffix(b"\n").unwrap_or(&output)) {
                Ok(found) if found == expected => (),
                Ok(_) => return Ok(false),
                Err(error) => return Err(unsure(&error).into()),
            },
            Ok(Err(_)) if no_repository(root) => return Ok(false),
            Ok(Err(refusal)) => return Err(unsure(&refusal).into()),
            Err(error) => return Err(unsure(&unanswered(&error)).into()),
        }
    }
    Ok(true)
}

/// Whether no repository can be at `root`: the folder is gone or holds no `.git`, its `.git` is
/// a link to nothing, which Git passes over, or its `.git` is a linked worktree's file whose
/// `gitdir:` names a Git folder that is gone, as after the main repository moved. Such a
/// folder is no checkout, so nothing there waits for Git.
fn no_repository(root: &Path) -> bool {
    let file = root.join(".git");
    match file.metadata() {
        Err(error) => missing(&error),
        // Git reads `gitdir: <folder>`, without the line end, relative to the worktree.
        Ok(metadata) if metadata.is_file() && metadata.len() <= 4096 => std::fs::read(&file)
            .ok()
            .and_then(|text| {
                let folder = text.strip_prefix(b"gitdir: ")?;
                let end = folder
                    .iter()
                    .rposition(|byte| !matches!(byte, b'\n' | b'\r'))?;
                Some(root.join(OsStr::from_bytes(&folder[..=end])))
            })
            .is_some_and(|folder| folder.metadata().is_err_and(|error| missing(&error))),
        Ok(_) => false,
    }
}

fn missing(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// Whether a repository keeps `file`, which is in a folder that is no longer the checkout's:
/// the nearest repository in a folder above it tracks it, or cannot say. A `.git` for which
/// `no_repository` holds is passed over, since a repository further up may still track the
/// file. Only with no repository above it does nothing keep it.
pub(super) async fn kept(file: &Path) -> bool {
    for root in file.ancestors().skip(1) {
        if no_repository(root) {
            continue;
        }
        let Some(relative) = file.strip_prefix(root).ok().and_then(Path::to_str) else {
            return true;
        };
        return tracked(root, &[relative]).await.unwrap_or(true);
    }
    false
}

/// Whether `folder` is in no Git checkout: neither it nor a folder above it can hold a
/// repository, by `no_repository`. Read only once Git refused, to say what the user does.
pub(super) fn outside_checkout(folder: &Path) -> bool {
    folder.ancestors().all(no_repository)
}

pub(super) async fn require_ignored(root: &Path, paths: &[&str]) -> Result<(), Failure> {
    for path in paths {
        let file = root.join(path);
        match git_run(root, &["check-ignore", "--no-index", "--quiet", "--", path]).await {
            Ok(Ok(_)) => (),
            // Exit code 1 is Git's answer that it does not ignore the path.
            Ok(Err(refusal)) if refusal.code == Some(1) => {
                return Err(
                    format!("Checkout setup requires Git to ignore {}.", file.display()).into(),
                );
            }
            Ok(Err(refusal)) => {
                return Err(format!(
                    "Git could not say whether it ignores {}: {refusal}",
                    file.display()
                )
                .into());
            }
            Err(error) => {
                return Err(format!(
                    "Git could not say whether it ignores {}: {}",
                    file.display(),
                    unanswered(&error)
                )
                .into());
            }
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
    /// folder now holds another repository, or one whose Git folder is gone.
    Elsewhere,
}

pub(super) async fn destination(
    file: &Path,
    common: &Path,
    relative: &str,
) -> Result<Destination, Failure> {
    let count = Path::new(relative).components().count();
    // A record that names no file at `relative` in a folder is damaged. With no worktree to
    // ask, the file counts as elsewhere, where only what proves to be ReviewQ's goes.
    let Some(worktree) = file
        .ancestors()
        .nth(count)
        .filter(|worktree| worktree.join(relative) == file)
    else {
        return Ok(Destination::Elsewhere);
    };
    Ok(if !belongs(worktree, common).await? {
        Destination::Elsewhere
    } else if tracked(worktree, &[relative]).await? {
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
    let output = answer(directory, &["rev-parse", "--path-format=absolute", name]).await?;
    path(output.strip_suffix(b"\n").unwrap_or(&output))
}

/// Git's output, or an error with Git's own words about `directory`.
async fn answer(directory: &Path, arguments: &[&str]) -> Result<Vec<u8>, Failure> {
    let cause = match git_run(directory, arguments).await {
        Ok(Ok(output)) => return Ok(output),
        Ok(Err(refusal)) => refusal.to_string(),
        Err(error) => unanswered(&error).to_owned(),
    };
    Err(Box::new(Unread {
        folder: directory.to_owned(),
        cause,
    }))
}

/// Why Git gave no answer, when it did not run to the end.
fn unanswered(error: &AppError) -> &'static str {
    match error {
        AppError::PayloadTooLarge => "its answer was too long",
        _ => "it did not start or did not answer within 4 seconds",
    }
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
