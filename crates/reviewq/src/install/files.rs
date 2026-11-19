use std::collections::BTreeMap;
use std::fs::{self, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{
    Failure, Release,
    bundle::{self, Bundle, Entry},
    paths::{self, Paths},
};

fn current(paths: &Paths) -> PathBuf {
    paths.profile.join("current")
}

fn release_target(release: &Release) -> PathBuf {
    PathBuf::from("releases").join(&release.id)
}

fn skill_target(paths: &Paths) -> PathBuf {
    current(paths).join("hosts/codex/skills/reviewq")
}

fn launcher_target(paths: &Paths) -> PathBuf {
    current(paths).join("bin/reviewq")
}

pub(super) fn unit_digest(path: &Path) -> Result<Option<String>, Failure> {
    if !paths::exists(path)? {
        return Ok(None);
    }
    let file = bundle::regular(path).map_err(|_| paths::conflict(path))?;
    paths::private(&file, path)?;
    if file.metadata()?.len() > 16 * 1024 {
        return Err(paths::conflict(path));
    }
    Ok(Some(bundle::digest_file(file)?))
}

pub(super) fn verify_current(paths: &Paths, release: &Release) -> Result<(), Failure> {
    let pointer = current(paths);
    if fs::read_link(&pointer).ok().as_ref() != Some(&release_target(release)) {
        return Err(paths::conflict(&pointer));
    }
    Ok(())
}

pub(super) fn preflight(
    paths: &Paths,
    old: Option<&Release>,
    next: Option<&Release>,
) -> Result<(), Failure> {
    let wanted = [old, next];
    allowed_link(
        &current(paths),
        wanted.into_iter().flatten().map(release_target),
    )?;
    allowed_link(
        &paths.launcher,
        wanted.into_iter().flatten().map(|_| launcher_target(paths)),
    )?;
    if let Some(existing) = unit_digest(&paths.unit)?
        && !wanted
            .into_iter()
            .flatten()
            .any(|release| release.unit == existing)
    {
        return Err(paths::conflict(&paths.unit));
    }
    for skill in wanted
        .into_iter()
        .flatten()
        .filter_map(|release| release.skill.as_deref())
    {
        allowed_link(
            skill,
            old.filter(|release| release.skill.as_deref() == Some(skill))
                .into_iter()
                .chain(next.filter(|release| release.skill.as_deref() == Some(skill)))
                .map(|_| skill_target(paths)),
        )?;
    }
    Ok(())
}

pub(super) fn check_new_skill(old: Option<&Release>, next: &Release) -> Result<(), Failure> {
    if let Some(skill) = next.skill.as_deref()
        && old.and_then(|release| release.skill.as_deref()) != Some(skill)
        && paths::exists(skill)?
    {
        return Err(paths::conflict(skill));
    }
    Ok(())
}

fn allowed_link(path: &Path, allowed: impl Iterator<Item = PathBuf>) -> Result<(), Failure> {
    if paths::exists(path)? {
        let actual = fs::read_link(path).map_err(|_| paths::conflict(path))?;
        if !allowed.into_iter().any(|target| target == actual) {
            return Err(paths::conflict(path));
        }
    }
    Ok(())
}

pub(super) fn reserve_stage(paths: &Paths) -> Result<String, Failure> {
    reserve_operation(paths, ".stage-")
}

pub(super) fn reserve_removal(paths: &Paths) -> Result<String, Failure> {
    reserve_operation(paths, ".remove-")
}

fn reserve_operation(paths: &Paths, prefix: &str) -> Result<String, Failure> {
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| "Could not reserve release staging.")?;
    let id = bundle::hex(&random);
    let path = operation_path(paths, prefix, &id)?;
    if paths::exists(&path)? {
        return Err(paths::conflict(&path));
    }
    Ok(id)
}

fn operation_path(paths: &Paths, prefix: &str, id: &str) -> Result<PathBuf, Failure> {
    if id.len() != 32 || !id.bytes().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("Invalid release staging identity.".into());
    }
    Ok(paths.profile.join("releases").join(format!("{prefix}{id}")))
}

pub(super) fn stage(paths: &Paths, bundle: &Bundle, id: &str) -> Result<(), Failure> {
    let releases = paths.profile.join("releases");
    let destination = releases.join(&bundle.id);
    let stage = operation_path(paths, ".stage-", id)?;
    if paths::exists(&stage)? {
        paths::ensure_dir(&stage, true)?;
        fs::remove_dir_all(&stage)?;
        paths::sync(&releases)?;
    }
    paths::ensure_dir(&stage, true)?;
    if paths::exists(&destination)? {
        if Bundle::read(&destination)?.id != bundle.id {
            return Err(paths::conflict(&destination));
        }
    } else {
        let staged_bundle = stage.join("bundle");
        paths::ensure_dir(&staged_bundle, true)?;
        for entry in &bundle.manifest.files {
            let path = staged_bundle.join(&entry.path);
            paths::ensure_dir(paths::parent(&path)?, true)?;
            copy(&bundle.root.join(&entry.path), &path, entry.mode)?;
        }
        copy(
            &bundle.root.join("manifest.json"),
            &staged_bundle.join("manifest.json"),
            0o600,
        )?;
        let staged = Bundle::read(&staged_bundle)?;
        if staged.id != bundle.id {
            return Err("The release changed while being staged.".into());
        }
        paths::sync(&staged_bundle)?;
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &staged_bundle,
            rustix::fs::CWD,
            &destination,
            rustix::fs::RenameFlags::NOREPLACE,
        )?;
        paths::sync(&releases)?;
    }
    for (name, entry) in assets(bundle) {
        let path = paths.profile.join("assets").join(name);
        if paths::exists(&path)? {
            verify(&path, entry)?;
        } else {
            paths::ensure_dir(paths::parent(&path)?, true)?;
            let staged_asset = stage.join("asset");
            copy(&destination.join(&entry.path), &staged_asset, entry.mode)?;
            verify(&staged_asset, entry)?;
            rustix::fs::renameat_with(
                rustix::fs::CWD,
                &staged_asset,
                rustix::fs::CWD,
                &path,
                rustix::fs::RenameFlags::NOREPLACE,
            )?;
            paths::sync(paths::parent(&path)?)?;
        }
    }
    fs::remove_dir(&stage)?;
    paths::sync(&releases)
}

fn copy(from: &Path, to: &Path, mode: u32) -> Result<(), Failure> {
    let mut source = bundle::regular(from)?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(to)?;
    destination.set_permissions(Permissions::from_mode(mode))?;
    io::copy(&mut source, &mut destination)?;
    destination.sync_all()?;
    paths::sync(paths::parent(to)?)
}

fn assets(bundle: &Bundle) -> impl Iterator<Item = (&str, &Entry)> {
    bundle
        .manifest
        .files
        .iter()
        .filter_map(|entry| Some((entry.path.strip_prefix("web/assets/")?, entry)))
}

fn verify(path: &Path, entry: &Entry) -> Result<(), Failure> {
    let file = bundle::regular(path).map_err(|_| paths::conflict(path))?;
    if file.metadata()?.len() != entry.size || bundle::digest_file(file)? != entry.sha256 {
        return Err(paths::conflict(path));
    }
    Ok(())
}

pub(super) fn set(
    paths: &Paths,
    old: Option<&Release>,
    next: &Release,
    unit: &[u8],
) -> Result<(), Failure> {
    preflight(paths, old, Some(next))?;
    ensure_link(&paths.launcher, &launcher_target(paths))?;
    let config = crate::runtime_settings::config_path(&paths.profile, false);
    if !paths::exists(&config)? {
        let body = std::env::var("WSL_DISTRO_NAME")
            .ok()
            .filter(|value| !value.is_empty())
            .map_or_else(
                || serde_json::json!({}),
                |wsl| serde_json::json!({"wslDistribution":wsl}),
            );
        paths::write_json(&config, &body)?;
    }
    if unit_digest(&paths.unit)?.as_deref() != Some(&next.unit) {
        if bundle::hex(&Sha256::digest(unit)) != next.unit {
            return Err("Service unit changed after planning.".into());
        }
        paths::atomic_file(&paths.unit, unit, paths::exists(&paths.unit)?)?;
    }
    ensure_link(&current(paths), &release_target(next))?;
    if let Some(previous) = old.and_then(|release| release.skill.as_deref())
        && next.skill.as_deref() != Some(previous)
    {
        remove_link(previous, &skill_target(paths))?;
    }
    if let Some(skill) = next.skill.as_deref() {
        paths::ensure_dir(paths::parent(skill)?, false)?;
        ensure_link(skill, &skill_target(paths))?;
    }
    Ok(())
}

fn ensure_link(path: &Path, target: &Path) -> Result<(), Failure> {
    let exists = paths::exists(path)?;
    if exists && fs::read_link(path)? == target {
        return Ok(());
    }
    paths::link(path, target, exists)
}

fn remove_link(path: &Path, target: &Path) -> Result<(), Failure> {
    allowed_link(path, std::iter::once(target.to_path_buf()))?;
    if paths::exists(path)? {
        fs::remove_file(path)?;
        paths::sync(paths::parent(path)?)?;
    }
    Ok(())
}

pub(super) fn remove(
    paths: &Paths,
    release: &Release,
    known: &[String],
    operation: &str,
) -> Result<Vec<PathBuf>, Failure> {
    let mut conflicts = Vec::new();
    if let Some(skill) = release.skill.as_deref()
        && remove_link(skill, &skill_target(paths)).is_err()
    {
        conflicts.push(skill.to_path_buf());
    }
    if unit_digest(&paths.unit)?.as_deref() == Some(&release.unit) {
        fs::remove_file(&paths.unit)?;
        paths::sync(paths::parent(&paths.unit)?)?;
    }
    let pointer = current(paths);
    if allowed_link(&pointer, std::iter::once(release_target(release))).is_err() {
        conflicts.push(pointer.clone());
    }
    if allowed_link(&paths.launcher, std::iter::once(launcher_target(paths))).is_err() {
        conflicts.push(paths.launcher.clone());
    }
    let mut owned_assets = BTreeMap::new();
    let mut owned_releases = Vec::new();
    for id in known {
        let directory = paths.profile.join("releases").join(id);
        if !paths::exists(&directory)? {
            continue;
        }
        let Ok(bundle) = Bundle::read(&directory) else {
            conflicts.push(directory);
            continue;
        };
        if &bundle.id != id {
            conflicts.push(directory);
            continue;
        }
        for (name, entry) in assets(&bundle) {
            let copy = paths.profile.join("assets").join(name);
            if paths::exists(&copy)? {
                owned_assets.insert(copy, (entry.size, entry.sha256.clone()));
            }
        }
        owned_releases.push((id, directory));
    }
    for (path, (size, digest)) in owned_assets {
        let valid = (|| -> Result<bool, Failure> {
            let file = bundle::regular(&path)?;
            Ok(file.metadata()?.len() == size && bundle::digest_file(file)? == digest)
        })()
        .unwrap_or(false);
        if valid {
            fs::remove_file(&path)?;
            paths::sync(paths::parent(&path)?)?;
        } else {
            conflicts.push(path);
        }
    }
    if !conflicts.is_empty() {
        return Ok(conflicts);
    }
    let tombstones = operation_path(paths, ".remove-", operation)?;
    paths::ensure_dir(&tombstones, true)?;
    for (id, directory) in owned_releases.iter().filter(|(id, _)| *id != &release.id) {
        quarantine(&tombstones, id, directory)?;
    }
    if let Some((id, directory)) = owned_releases.iter().find(|(id, _)| *id == &release.id) {
        quarantine(&tombstones, id, directory)?;
    }
    fs::remove_dir_all(&tombstones)?;
    paths::sync(paths::parent(&tombstones)?)?;
    remove_link(&pointer, &release_target(release))?;
    remove_link(&paths.launcher, &launcher_target(paths))?;
    Ok(conflicts)
}

fn quarantine(tombstones: &Path, id: &str, source: &Path) -> Result<(), Failure> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        tombstones.join(id),
        rustix::fs::RenameFlags::NOREPLACE,
    )?;
    paths::sync(tombstones)?;
    paths::sync(paths::parent(source)?)
}
