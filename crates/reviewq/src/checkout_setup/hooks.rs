use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use reviewq_providers::git_run;
use sha2::{Digest, Sha256};

use super::files::{self, Edit, read};
use super::git::Checkout;
use super::{Failure, MARKER, Unfinished};

const POST_CHECKOUT: &str = "hooks/post-checkout";

/// Writes the hook for `checkout`, unless Git uses another hooks folder or the hook there is
/// not ReviewQ's. A record ReviewQ cannot read stops setup with its step, then `again`.
pub(super) async fn reconcile(
    profile: &Path,
    checkout: &Checkout,
    again: &str,
) -> Result<(), Failure> {
    let Some(launcher) = launcher().filter(|launcher| launcher.is_file()) else {
        return Ok(());
    };
    // Git applies includes and per-worktree overrides in their native scope. Exit code 1 is
    // Git's answer that the key is not set; anything else leaves the hooks folder alone.
    for root in &checkout.worktrees {
        match git_run(root, &["config", "--get", "core.hooksPath"]).await {
            Ok(Err(refusal)) if refusal.code == Some(1) => (),
            _ => return Ok(()),
        }
    }
    let hook = checkout.common.join(POST_CHECKOUT);
    let inventory = files::hook_inventory(profile, &checkout.common)?;
    let script = script(&launcher, profile, &checkout.common)?;
    let owned = load(&inventory, again)?.unwrap_or_default();
    let previous = match hook.symlink_metadata() {
        Ok(_) => match read(&hook) {
            Ok(Some(file)) if file.mode == 0o700 && ours(&file.bytes, &owned, Some(&script)) => {
                Some(file)
            }
            _ => return Ok(()),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let digest = digest(script.as_bytes());
    // Until the write lands, the hook is either the one there or the new one.
    let mut next = BTreeSet::from([digest]);
    next.extend(previous.as_ref().map(|file| self::digest(&file.bytes)));
    save(&inventory, &next)?;
    Edit::executable(hook, previous, script.into_bytes()).apply()?;
    save(&inventory, &BTreeSet::from([digest]))?;
    Ok(())
}

/// Removes the hook ReviewQ wrote. With a record, one whose bytes are none ReviewQ wrote but
/// that still holds ReviewQ's marker and names this profile and this checkout's Git folder was
/// changed by hand: it stays, and the error says which lines to remove, then `again`, the step
/// that finishes the command. Any other, such as another profile's hook, which also holds the
/// marker, is not this profile's, as setup also leaves it alone: the record goes and the file
/// stays. Without a record only the script ReviewQ writes for this checkout goes.
pub(super) fn cleanup(profile: &Path, common: &Path, again: &str) -> Result<(), Failure> {
    let inventory = files::hook_inventory(profile, common)?;
    let recorded = load(&inventory, again)?;
    let path = common.join(POST_CHECKOUT);
    let script = launcher().and_then(|launcher| script(&launcher, profile, common).ok());
    if recorded.is_none() && script.is_none() {
        return Ok(());
    }
    let empty = BTreeSet::new();
    let owned = recorded.as_ref().unwrap_or(&empty);
    let mut left = Unfinished::new(again);
    match files::read_written(&path) {
        Ok(Some(file)) if ours(&file.bytes, owned, script.as_deref()) => {
            if let Err(error) = Edit::remove(path.clone(), file).apply() {
                left.failed("remove its Git hook", &path, &error);
            }
        }
        Ok(Some(file)) if recorded.is_some() && edited(&file.bytes, profile, common) => {
            left.step(format!(
                "ReviewQ left its Git hook in place in {} because it no longer matches what ReviewQ wrote. To finish, remove the line \"{MARKER}\" there and the `if` line after it.",
                path.display()
            ));
        }
        Ok(_) => (),
        Err(error) if recorded.is_some() => left.failed("read its Git hook", &path, &error),
        Err(_) => (),
    }
    left.result()?;
    if let Some(file) = read(&inventory)? {
        Edit::remove(inventory, file).apply()?;
    }
    Ok(())
}

/// Whether `bytes` are this profile's hook for the checkout at `common` changed by hand: they
/// still hold ReviewQ's marker line and the arguments that name both.
fn edited(bytes: &[u8], profile: &Path, common: &Path) -> bool {
    let named = |argument: Result<String, Failure>| {
        argument.is_ok_and(|argument| {
            bytes
                .windows(argument.len())
                .any(|piece| piece == argument.as_bytes())
        })
    };
    bytes
        .split(|byte| *byte == b'\n')
        .any(|line| line == MARKER.as_bytes())
        && named(profile_argument(profile))
        && named(common_argument(common))
}

/// Whether `bytes` are a hook ReviewQ wrote: one the record names, or the script ReviewQ
/// writes for this checkout now, which no one else writes.
fn ours(bytes: &[u8], owned: &BTreeSet<[u8; 32]>, script: Option<&str>) -> bool {
    owned.contains(&digest(bytes)) || script.is_some_and(|script| script.as_bytes() == bytes)
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The hooks ReviewQ wrote, or `None` when it has no record. A record ReviewQ cannot read as
/// one it wrote is rebuilt from the hook file: it is then empty, and `ours` still knows the
/// script ReviewQ writes. A file it cannot open at all is the user's to fix or delete.
fn load(path: &Path, again: &str) -> Result<Option<BTreeSet<[u8; 32]>>, Failure> {
    let file = match read(path) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(None),
        Err(error) => {
            let mut left = Unfinished::new(again);
            left.step(format!(
                "ReviewQ could not read its Git hook record {}: {}. To finish, delete that file.",
                path.display(),
                error.to_string().trim_end_matches('.')
            ));
            return Err(Box::new(left));
        }
    };
    let owned = serde_json::from_slice::<BTreeSet<[u8; 32]>>(&file.bytes)
        .ok()
        .filter(|owned| file.mode == 0o600 && !owned.is_empty() && owned.len() <= 4);
    Ok(Some(owned.unwrap_or_default()))
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

/// Where `reviewq install` puts the launcher the hook runs.
fn launcher() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    home.is_absolute().then(|| home.join(".local/bin/reviewq"))
}

/// The hook ReviewQ writes for the checkout whose Git folder is `common`.
fn script(launcher: &Path, profile: &Path, common: &Path) -> Result<String, Failure> {
    let settings = crate::runtime_settings::config_path(profile, false);
    Ok(format!(
        "#!/bin/sh\n{MARKER}\nif [ \"${{3-}}\" = 1 ]; then {} {} internal setup-worktree --settings {} {} </dev/null >/dev/null || :; fi\n",
        literal(launcher)?,
        profile_argument(profile)?,
        literal(&settings)?,
        common_argument(common)?,
    ))
}

/// The hook's argument naming the profile, quoted, so one profile's never matches another's.
fn profile_argument(profile: &Path) -> Result<String, Failure> {
    Ok(format!("--profile {}", literal(profile)?))
}

/// The hook's argument naming the checkout's Git folder, quoted as `profile_argument`.
fn common_argument(common: &Path) -> Result<String, Failure> {
    Ok(format!("--common-dir {}", literal(common)?))
}

fn literal(path: &Path) -> Result<String, Failure> {
    let value = path
        .to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .ok_or("Invalid path for the managed Git hook.")?;
    Ok(format!("'{}'", value.replace("'", "'\"'\"'")))
}
