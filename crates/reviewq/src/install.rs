use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use reviewq_domain::session::AbsolutePath;
use reviewq_store::SqliteStore;
use serde::{Deserialize, Serialize};
use sha2::Digest;

mod bundle;
mod claude;
mod files;
pub(crate) mod paths;
use crate::{runtime_settings, service};
use bundle::Bundle;
use paths::Paths;

pub(crate) type Failure = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn verify_bundle(path: &Path) -> Result<(), Failure> {
    let bundle = Bundle::read(path)?;
    println!("{} {}", bundle.manifest.version, bundle.id);
    Ok(())
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Release {
    id: String,
    pub binary: String,
    pub store: i64,
    unit: String,
    skill: Option<PathBuf>,
    #[serde(default)]
    claude: Option<claude::Registration>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    next: Release,
    was_enabled: bool,
    was_running: bool,
    store_id: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Inventory {
    active: Option<Release>,
    pending: Option<Activation>,
    known: Vec<String>,
    removing: bool,
}

fn journal(paths: &Paths) -> PathBuf {
    paths.state.join("install.json")
}

fn load(paths: &Paths) -> Result<Inventory, Failure> {
    Ok(paths::read_json(&journal(paths))?.unwrap_or_default())
}

pub(crate) fn installed(paths: &Paths) -> Result<Option<Release>, Failure> {
    let inventory = load(paths)?;
    if inventory.pending.is_some() || inventory.removing {
        return Err("Complete the interrupted ReviewQ installation or removal first.".into());
    }
    Ok(inventory.active)
}

pub(crate) fn active(paths: &Paths) -> Result<Release, Failure> {
    let active = installed(paths)?.ok_or("ReviewQ is not installed for this profile.")?;
    files::verify_current(paths, &active)?;
    if files::unit_digest(&paths.unit)?.as_deref() != Some(&active.unit) {
        return Err(paths::conflict(&paths.unit));
    }
    Ok(active)
}

pub(crate) async fn install(bundle_path: &Path) -> Result<(), Failure> {
    let bundle = Bundle::read(bundle_path)?;
    let paths = Paths::resolve()?;
    let _lock = paths.lock()?;
    let settings = runtime_settings::Settings::load(&paths.profile, false)?;
    let mut inventory = load(&paths)?;
    if inventory.removing {
        return Err("Finish the interrupted uninstall before installing a release.".into());
    }
    service::manager().await?;
    service::unit_location(&paths).await?;
    let unit = service::unit(&paths, &bundle.root.join("packaging/reviewq.service.in"))?;
    let skill = native_command(&paths, "codex", settings.codex_command.as_deref())
        .map(|_| paths.codex_home.join("skills/reviewq"));
    let claude = native_command(&paths, "claude", settings.claude_command.as_deref())
        .map(|executable| claude::plan(&paths, &bundle, executable))
        .transpose()?;
    let next = Release {
        id: bundle.id.clone(),
        binary: bundle
            .manifest
            .files
            .iter()
            .find(|item| item.path == "bin/reviewq")
            .ok_or("Missing executable.")?
            .sha256
            .clone(),
        store: bundle.manifest.store,
        unit: bundle::hex(&sha2::Sha256::digest(&unit)),
        skill,
        claude,
    };
    if let Some(host) = &next.claude {
        claude::validate(&paths, &bundle, host).await?;
    }
    let old = inventory.active.as_ref();
    let pending = match inventory.pending.as_ref() {
        Some(pending) if pending.next == next => pending,
        Some(_) => {
            return Err("Retry the exact pending bundle before selecting another release.".into());
        }
        None => {
            files::preflight(&paths, old, None)?;
            files::check_new_skill(old, &next)?;
            claude::preflight(&paths, old, &next, false).await?;
            let (was_enabled, was_running) = service::state().await?;
            let store_id = if was_running {
                let previous = old.ok_or("Preserved an unrelated running user service.")?;
                Some(
                    service::ready(&paths, &previous.binary, previous.store)
                        .await?
                        .store_id,
                )
            } else {
                probe(&paths, next.store).await?
            };
            paths.prepare()?;
            if !inventory.known.iter().any(|id| id == &next.id) {
                inventory.known.push(next.id.clone());
            }
            inventory.pending = Some(Activation {
                next: next.clone(),
                was_enabled,
                was_running,
                store_id,
            });
            paths::write_json(&journal(&paths), &inventory)?;
            inventory
                .pending
                .as_ref()
                .ok_or("Activation was not recorded.")?
        }
    };
    let expected_store = pending.store_id.clone();
    let old = inventory.active.as_ref();
    files::preflight(&paths, old, Some(&next))?;
    claude::preflight(&paths, old, &next, true).await?;
    files::stage(&paths, &bundle)?;
    let result = activate(&paths, old, &next, &unit, expected_store.as_deref()).await;
    let identity = match result {
        Ok(identity) => identity,
        Err(error) => {
            if let Some(previous) = old {
                rollback(&paths, previous, pending).await?;
                inventory.pending = None;
                paths::write_json(&journal(&paths), &inventory)?;
            }
            return Err(error);
        }
    };
    inventory.active = Some(next.clone());
    inventory.pending = None;
    paths::write_json(&journal(&paths), &inventory)?;
    let endpoint =
        crate::runtime::read_endpoint_with_build(&paths.profile, &identity.build_digest)?;
    println!(
        "ReviewQ {}\nBundle {}\nDashboard http://{}/\nRuntime configuration {}",
        bundle.manifest.version,
        next.id,
        endpoint.browser(),
        runtime_settings::config_path(&paths.profile, false).display(),
    );
    if next.skill.is_none() {
        println!(
            "Codex unavailable. Set codexCommand in runtime.json and retry install to enable its skill."
        );
    }
    if next.claude.is_none() {
        println!("Claude Code unavailable. Set claudeCommand in runtime.json and retry install.");
    } else {
        println!("Use /reload-plugins in open Claude Code sessions to load this release.");
    }
    Ok(())
}

async fn activate(
    paths: &Paths,
    old: Option<&Release>,
    next: &Release,
    unit: &[u8],
    store_id: Option<&str>,
) -> Result<reviewq_transport::ServiceIdentity, Failure> {
    if service::state().await?.1 {
        service::switch("stop").await?;
    }
    let stopped_store = probe(paths, next.store).await?;
    if store_id.is_some_and(|id| stopped_store.as_deref() != Some(id)) {
        return Err("The retained data changed during activation.".into());
    }
    files::set(paths, old, next, unit)?;
    claude::activate(paths, old, next).await?;
    service::reload().await?;
    service::unit_location(paths).await?;
    service::switch("enable").await?;
    service::switch("start").await?;
    let identity = service::ready(paths, &next.binary, next.store).await?;
    if stopped_store.is_some_and(|id| id != identity.store_id) {
        return Err("The started service did not retain the verified store.".into());
    }
    Ok(identity)
}

async fn rollback(paths: &Paths, previous: &Release, pending: &Activation) -> Result<(), Failure> {
    files::preflight(paths, Some(previous), Some(&pending.next))?;
    let bundle = Bundle::read(&paths.profile.join("releases").join(&previous.id))?;
    if bundle.id != previous.id {
        return Err("The previous release could not be verified.".into());
    }
    let unit = service::unit(paths, &bundle.root.join("packaging/reviewq.service.in"))?;
    use sha2::{Digest, Sha256};
    if bundle::hex(&Sha256::digest(&unit)) != previous.unit {
        return Err("The previous service unit no longer matches its installation.".into());
    }
    if service::state().await?.1 {
        service::switch("stop").await?;
    }
    probe(paths, previous.store).await?;
    files::set(paths, Some(&pending.next), previous, &unit)?;
    if previous.claude.is_some() {
        claude::activate(paths, Some(&pending.next), previous).await?;
    } else {
        claude::remove(paths, &pending.next).await?;
    }
    service::reload().await?;
    service::switch(if pending.was_enabled {
        "enable"
    } else {
        "disable"
    })
    .await?;
    if pending.was_running {
        service::switch("start").await?;
        service::ready(paths, &previous.binary, previous.store).await?;
    }
    Ok(())
}

pub(crate) async fn uninstall() -> Result<(), Failure> {
    let paths = Paths::resolve()?;
    let _lock = paths.lock()?;
    let mut inventory = load(&paths)?;
    if inventory.pending.is_some() {
        return Err("Finish the interrupted activation before uninstalling.".into());
    }
    let Some(active) = inventory.active.as_ref() else {
        println!(
            "ReviewQ is already removed. Retained data {}",
            paths.profile.display()
        );
        return Ok(());
    };
    service::manager().await?;
    service::unit_location(&paths).await?;
    if files::unit_digest(&paths.unit)?.is_some_and(|digest| digest != active.unit) {
        return Err(paths::conflict(&paths.unit));
    }
    inventory.removing = true;
    paths::write_json(&journal(&paths), &inventory)?;
    if service::state().await?.1 {
        service::switch("stop").await?;
    }
    if service::state().await?.0 {
        service::switch("disable").await?;
    }
    claude::remove(&paths, active).await?;
    let conflicts = files::remove(&paths, active, &inventory.known)?;
    service::reload().await?;
    if !conflicts.is_empty() {
        return Err(format!(
            "Preserved modified installation entries: {}",
            conflicts
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
    }
    inventory = Inventory::default();
    paths::write_json(&journal(&paths), &inventory)?;
    println!(
        "ReviewQ removed. Retained data at {} and runtime settings at {}.",
        paths.profile.display(),
        runtime_settings::config_path(&paths.profile, false).display()
    );
    Ok(())
}

async fn probe(paths: &Paths, expected: i64) -> Result<Option<String>, Failure> {
    let database = paths.profile.join("reviewq.sqlite3");
    match database.symlink_metadata() {
        Ok(_) => {
            let identity = SqliteStore::probe(AbsolutePath::from_canonical(database.clone())?)
                .await
                .map_err(|error| format!("Cannot verify {} ({error}).", database.display()))?;
            if identity.format != expected {
                return Err(format!("Unsupported store format at {}.", database.display()).into());
            }
            Ok(Some(identity.store_id.to_string()))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            for name in [
                "reviewq.sqlite3-wal",
                "reviewq.sqlite3-shm",
                "reviewq.sqlite3-journal",
            ] {
                let sidecar = paths.profile.join(name);
                if paths::exists(&sidecar)? {
                    return Err(paths::conflict(&sidecar));
                }
            }
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn native_command(paths: &Paths, provider: &str, configured: Option<&str>) -> Option<PathBuf> {
    let default = paths.home.join(".local/bin").join(provider);
    runtime_settings::executable(configured.or_else(|| default.to_str())?)
}
