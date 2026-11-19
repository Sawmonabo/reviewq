use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use reviewq_app::access::AppError;
use reviewq_providers::git_output;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Failure;
use super::files::{self, Changes, Edit, read};
use super::git::Checkout;

const POST: &str = "post-checkout";
const INCLUDE: &str =
    "\n# ReviewQ Git hooks\n[include]\n\tpath = reviewq/hooks.config\n# End ReviewQ Git hooks\n";
const HOOK_NAMES: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
];

#[derive(Deserialize, Serialize)]
struct OwnedHooks {
    original: Option<PathBuf>,
    post_digests: BTreeSet<[u8; 32]>,
}

pub(super) async fn reconcile(profile: &Path, checkout: &Checkout) -> Result<(), Failure> {
    let Some(launcher) = installed_launcher() else {
        return Ok(());
    };
    let settings = crate::runtime_settings::config_path(profile, false);
    let record = files::hook_inventory(profile, &checkout.common)?;
    let previous = load(&record)?;
    let custom = optional_config(&checkout.primary, &["--path", "--get", "core.hooksPath"]).await?;
    reject_worktree_overrides(checkout).await?;
    let forwarded = checkout.common.join("reviewq/hooks");
    let original = if let Some(previous) = &previous {
        let expected = previous.original.as_ref().map(|_| forwarded.as_path());
        let current = custom.as_ref().map(Path::new);
        let original_default = current.is_none()
            && previous.original.as_deref() == Some(checkout.common.join("hooks").as_path());
        if expected != current && current != previous.original.as_deref() && !original_default {
            return Err(conflict(&checkout.common.join("config")));
        }
        previous.original.clone()
    } else if let Some(custom) = custom {
        let configured = PathBuf::from(custom);
        if configured == forwarded {
            return Err(conflict(&checkout.common.join("config")));
        }
        Some(configured)
    } else if checkout
        .common
        .join("hooks")
        .join(POST)
        .symlink_metadata()
        .is_ok()
    {
        Some(checkout.common.join("hooks"))
    } else {
        None
    };

    let mut owned = OwnedHooks {
        original,
        post_digests: previous
            .as_ref()
            .map_or_else(BTreeSet::new, |old| old.post_digests.clone()),
    };
    let directory = hook_dir(&checkout.common, &owned);
    let post_path = directory.join(POST);
    let before = existing(&post_path, &owned.post_digests)?;
    let script = script(
        owned.original.as_deref(),
        &launcher,
        profile,
        &settings,
        &checkout.common,
    )?;
    let digest = Sha256::digest(&script).into();
    owned.post_digests.insert(digest);
    if owned.post_digests.len() > 4 {
        return Err(conflict(&record));
    }
    let mut edits = vec![Edit::executable(post_path, before, script)];
    let links = forwarding_links(&directory, owned.original.as_deref());
    let missing = links
        .iter()
        .filter_map(|link| match link.present() {
            Ok(true) if previous.is_some() => None,
            Ok(true) => Some(Err(conflict(&link.path))),
            Ok(false) => Some(Ok(link)),
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    if owned.original.is_some() {
        let path = checkout.common.join("reviewq/hooks.config");
        let bytes = git_config(&forwarded)?;
        let before = read(&path)?;
        if before
            .as_ref()
            .is_some_and(|file| file.bytes != bytes || previous.is_none())
        {
            return Err(conflict(&path));
        }
        edits.push(Edit::new(path, before, bytes, true));
    }
    save(&record, &owned)?;
    let mut changes = Changes::new(edits);
    changes.apply()?;
    if owned.original.is_some() {
        let mut created = Vec::new();
        let mut installed_include = false;
        let result = async {
            for link in missing {
                symlink(&link.target, &link.path)?;
                created.push(link);
            }
            installed_include = include(&checkout.common, true)?;
            let effective =
                optional_config(&checkout.primary, &["--path", "--get", "core.hooksPath"]).await?;
            if effective.as_ref().map(Path::new) != Some(forwarded.as_path()) {
                return Err(conflict(&checkout.common.join("config")));
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if installed_include {
                include(&checkout.common, false)?;
            }
            for link in created {
                link.remove()?;
            }
            changes.rollback()?;
            return Err(error);
        }
    }
    owned.post_digests = BTreeSet::from([digest]);
    save(&record, &owned)?;
    Ok(())
}

pub(super) fn cleanup(profile: &Path, common: &Path) -> Result<(), Failure> {
    let record = files::hook_inventory(profile, common)?;
    let Some(owned) = load(&record)? else {
        return Ok(());
    };
    let directory = hook_dir(common, &owned);
    let post_path = directory.join(POST);
    let post = existing(&post_path, &owned.post_digests)?.map(|file| Edit::remove(post_path, file));
    let links = forwarding_links(&directory, owned.original.as_deref());
    if owned.original.is_some() {
        for link in &links {
            link.present()?;
        }
        let hook_config = common.join("reviewq/hooks.config");
        let old = read(&hook_config)?;
        let expected = git_config(&directory)?;
        if old.as_ref().is_some_and(|file| file.bytes != expected) {
            return Err(conflict(&hook_config));
        }
        include(common, false)?;
        if let Some(file) = old {
            Edit::remove(hook_config, file).apply()?;
        }
        for link in links {
            link.remove()?;
        }
    }
    if let Some(post) = post {
        post.apply()?;
    }
    if let Some(file) = read(&record)? {
        Edit::remove(record, file).apply()?;
    }
    Ok(())
}

struct ForwardingLink {
    path: PathBuf,
    target: PathBuf,
}

impl ForwardingLink {
    fn present(&self) -> Result<bool, Failure> {
        let parent = self
            .path
            .parent()
            .ok_or("Missing private hook directory.")?;
        match self.path.symlink_metadata() {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Ok(meta)
                if meta.is_symlink()
                    && meta.uid() == rustix::process::getuid().as_raw()
                    && parent.canonicalize()? == parent
                    && self.path.read_link()? == self.target =>
            {
                Ok(true)
            }
            _ => Err(conflict(&self.path)),
        }
    }

    fn remove(&self) -> Result<(), Failure> {
        if self.present()? {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

fn forwarding_links(directory: &Path, original: Option<&Path>) -> Vec<ForwardingLink> {
    let Some(original) = original else {
        return Vec::new();
    };
    let target = if original.is_absolute() {
        original.to_owned()
    } else {
        Path::new("/proc/self/cwd").join(original)
    };
    // Git performs its normal existence and executable checks on the original entry.
    // A missing original stays missing through the dangling symlink.
    HOOK_NAMES
        .iter()
        .filter(|name| **name != POST)
        .map(|name| ForwardingLink {
            path: directory.join(name),
            target: target.join(name),
        })
        .collect()
}

fn existing(
    path: &Path,
    expected: &BTreeSet<[u8; 32]>,
) -> Result<Option<files::Existing>, Failure> {
    let existing = read(path)?;
    if existing.as_ref().is_some_and(|file| {
        let actual: [u8; 32] = Sha256::digest(&file.bytes).into();
        file.mode != 0o700 || !expected.contains(&actual)
    }) {
        return Err(conflict(path));
    }
    Ok(existing)
}

fn save(record: &Path, owned: &OwnedHooks) -> Result<(), Failure> {
    Edit::new(
        record.to_owned(),
        read(record)?,
        serde_json::to_vec(owned)?,
        true,
    )
    .apply()
}

fn load(record: &Path) -> Result<Option<OwnedHooks>, Failure> {
    let Some(file) = read(record)? else {
        return Ok(None);
    };
    if file.mode != 0o600 {
        return Err(conflict(record));
    }
    let owned: OwnedHooks = serde_json::from_slice(&file.bytes)?;
    if owned.post_digests.is_empty() || owned.post_digests.len() > 4 {
        return Err(conflict(record));
    }
    Ok(Some(owned))
}

fn hook_dir(common: &Path, owned: &OwnedHooks) -> PathBuf {
    if owned.original.is_some() {
        common.join("reviewq/hooks")
    } else {
        common.join("hooks")
    }
}

async fn optional_config(root: &Path, args: &[&str]) -> Result<Option<String>, Failure> {
    let mut command = vec!["config"];
    command.extend(args);
    match git_output(root, &command).await {
        Ok(value) => {
            let value = std::str::from_utf8(&value)?
                .strip_suffix('\n')
                .ok_or("Invalid Git hook configuration.")?;
            if value.is_empty() || value.chars().any(char::is_control) {
                return Err("Invalid Git hook configuration.".into());
            }
            Ok(Some(value.to_owned()))
        }
        Err(AppError::NotFound) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn reject_worktree_overrides(checkout: &Checkout) -> Result<(), Failure> {
    if optional_config(
        &checkout.primary,
        &["--bool", "--get", "extensions.worktreeConfig"],
    )
    .await?
    .as_deref()
        == Some("true")
    {
        for root in &checkout.worktrees {
            if optional_config(root, &["--worktree", "--get", "core.hooksPath"])
                .await?
                .is_some()
            {
                return Err(format!(
                    "ReviewQ cannot change the worktree hook override in {}.",
                    root.display()
                )
                .into());
            }
        }
    }
    Ok(())
}

fn installed_launcher() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let launcher = home.join(".local/bin/reviewq");
    (home.is_absolute() && launcher.is_file()).then_some(launcher)
}

fn literal(path: &Path) -> Result<String, Failure> {
    let value = path
        .to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .ok_or("Invalid path for the managed Git hook.")?;
    Ok(format!("'{}'", value.replace("'", "'\"'\"'")))
}

fn script(
    original: Option<&Path>,
    launcher: &Path,
    profile: &Path,
    settings: &Path,
    common: &Path,
) -> Result<Vec<u8>, Failure> {
    let mut data = "#!/bin/sh\n# Managed by ReviewQ\n".to_owned();
    if let Some(original) = original {
        let target = literal(&original.join(POST))?;
        let target = if original.is_absolute() {
            target
        } else {
            format!("./{target}")
        };
        data.push_str(&format!(
            "original={target}\nif [ -x \"$original\" ]; then \"$original\" \"$@\" || exit $?; fi\n"
        ));
    }
    data.push_str(&format!(
        "if [ \"${{3-}}\" = 1 ]; then {} --profile {} internal setup-worktree --settings {} --common-dir {} </dev/null >/dev/null || :; fi\n",
        literal(launcher)?, literal(profile)?, literal(settings)?, literal(common)?
    ));
    data.push_str("exit 0\n");
    Ok(data.into_bytes())
}

fn git_config(directory: &Path) -> Result<Vec<u8>, Failure> {
    let path = directory
        .to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .ok_or("Invalid managed hook directory.")?;
    let path = path.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("[core]\n\thooksPath = \"{path}\"\n").into_bytes())
}

fn include(common: &Path, install: bool) -> Result<bool, Failure> {
    let path = common.join("config");
    let before = read(&path)?.ok_or_else(|| conflict(&path))?;
    let block = INCLUDE.as_bytes();
    let start = before
        .bytes
        .windows(block.len())
        .position(|value| value == block);
    let mut updated = before.bytes.clone();
    match (install, start) {
        (true, None)
            if !updated
                .windows(b"# ReviewQ Git hooks".len())
                .any(|v| v == b"# ReviewQ Git hooks") =>
        {
            updated.extend_from_slice(block)
        }
        (false, Some(start)) => {
            updated.drain(start..start + block.len());
        }
        (true, Some(_)) => return Ok(false),
        (false, None)
            if !updated
                .windows(b"# ReviewQ Git hooks".len())
                .any(|v| v == b"# ReviewQ Git hooks") =>
        {
            return Ok(false);
        }
        _ => return Err(conflict(&path)),
    }
    // Use Git's own config lock convention so other git config writers serialize.
    let lock = common.join("config.lock");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock)?;
    let mut locked = ConfigLock { path: lock, file };
    if read(&path)?
        .is_none_or(|current| current.bytes != before.bytes || current.mode != before.mode)
    {
        return Err(conflict(&path));
    }
    locked
        .file
        .set_permissions(fs::Permissions::from_mode(before.mode))?;
    locked.file.write_all(&updated)?;
    locked.file.sync_all()?;
    fs::rename(&locked.path, &path)?;
    locked.path.clear();
    Ok(true)
}

struct ConfigLock {
    path: PathBuf,
    file: fs::File,
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            match fs::remove_file(&self.path) {
                Ok(()) => (),
                Err(error) if error.kind() == ErrorKind::NotFound => (),
                Err(_) => (),
            }
        }
    }
}

fn conflict(path: &Path) -> Failure {
    format!(
        "ReviewQ could not reconcile the Git hook at {}. Its contents were preserved.",
        path.display()
    )
    .into()
}
