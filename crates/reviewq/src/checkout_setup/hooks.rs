use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use reviewq_app::access::AppError;
use reviewq_providers::git_output;
use sha2::{Digest, Sha256};

use super::Failure;
use super::files::{self, Edit, read};
use super::git::Checkout;

const POST_CHECKOUT: &str = "hooks/post-checkout";

/// Returns whether the service should observe the checkout's worktree metadata.
pub(super) async fn reconcile(profile: &Path, checkout: &Checkout) -> Result<bool, Failure> {
    let Some(launcher) = installed_launcher() else {
        return Ok(true);
    };
    // Git applies includes and per-worktree overrides in their native scope.
    for root in &checkout.worktrees {
        match git_output(root, &["config", "--get", "core.hooksPath"]).await {
            Err(AppError::NotFound) => (),
            _ => return Ok(true),
        }
    }
    let hook = checkout.common.join(POST_CHECKOUT);
    let inventory = files::hook_inventory(profile, &checkout.common)?;
    let mut owned = load(&inventory)?;
    let previous = match hook.symlink_metadata() {
        Ok(_) => match owned_file(&hook, &owned) {
            Ok(file) => file,
            Err(_) => return Ok(true),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let settings = crate::runtime_settings::config_path(profile, false);
    let script = format!(
        "#!/bin/sh\n# Managed by ReviewQ\nif [ \"${{3-}}\" = 1 ]; then {} --profile {} internal setup-worktree --settings {} --common-dir {} </dev/null >/dev/null || :; fi\n",
        literal(&launcher)?,
        literal(profile)?,
        literal(&settings)?,
        literal(&checkout.common)?,
    );
    let digest = Sha256::digest(script.as_bytes()).into();
    owned.insert(digest);
    if owned.len() > 4 {
        return Err(conflict(&hook));
    }
    save(&inventory, &owned)?;
    Edit::executable(hook, previous, script.into_bytes()).apply()?;
    save(&inventory, &BTreeSet::from([digest]))?;
    Ok(false)
}

pub(super) fn cleanup(profile: &Path, common: &Path) -> Result<(), Failure> {
    let inventory = files::hook_inventory(profile, common)?;
    let owned = load(&inventory)?;
    if owned.is_empty() {
        return Ok(());
    }
    let path = common.join(POST_CHECKOUT);
    if let Some(file) = owned_file(&path, &owned)? {
        Edit::remove(path, file).apply()?;
    }
    if let Some(file) = read(&inventory)? {
        Edit::remove(inventory, file).apply()?;
    }
    Ok(())
}

fn owned_file(path: &Path, owned: &BTreeSet<[u8; 32]>) -> Result<Option<files::Existing>, Failure> {
    let existing = read(path)?;
    if existing.as_ref().is_some_and(|file| {
        let digest: [u8; 32] = Sha256::digest(&file.bytes).into();
        file.mode != 0o700 || !owned.contains(&digest)
    }) {
        return Err(conflict(path));
    }
    Ok(existing)
}

fn load(path: &Path) -> Result<BTreeSet<[u8; 32]>, Failure> {
    let Some(file) = read(path)? else {
        return Ok(BTreeSet::new());
    };
    let owned: BTreeSet<[u8; 32]> = serde_json::from_slice(&file.bytes)?;
    if file.mode != 0o600 || owned.is_empty() || owned.len() > 4 {
        return Err(conflict(path));
    }
    Ok(owned)
}

fn save(path: &Path, owned: &BTreeSet<[u8; 32]>) -> Result<(), Failure> {
    Edit::new(
        path.to_owned(),
        read(path)?,
        serde_json::to_vec(owned)?,
        true,
    )
    .apply()
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

fn conflict(path: &Path) -> Failure {
    format!(
        "ReviewQ could not remove the modified Git hook or receipt at {}.",
        path.display()
    )
    .into()
}
