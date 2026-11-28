use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use reviewq_app::hosts::HookApproval;
use reviewq_domain::session::AbsolutePath;
use reviewq_store::SqliteStore;
use serde::{Deserialize, Serialize};
use sha2::Digest;

pub(crate) mod bridge;
mod bundle;
mod claude;
mod codex_hook;
pub(crate) mod data;
mod files;
mod managed;
pub(crate) mod paths;
mod retention;
pub(crate) mod start_door;
pub(crate) mod uninstall;
mod windows;
use crate::{native_client, runtime_settings, service};
use bundle::Bundle;
pub(crate) use managed::{Managed, StartupPermission};
use paths::Paths;
pub(crate) use retention::PREVIOUS_ASSETS;
use service::StartupCheck;

pub(crate) type Failure = Box<dyn std::error::Error + Send + Sync>;

/// A failure the person can act on, whose text says what to do, such as a conflicting entry
/// or an interrupted activation. Every other `Failure` is internal.
#[derive(Debug)]
pub(crate) struct Refusal(pub(crate) String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

pub(crate) fn refused(message: impl Into<String>) -> Failure {
    Box::new(Refusal(message.into()))
}

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
    #[serde(default)]
    windows: Option<windows::Registration>,
}

impl Release {
    pub(crate) fn windows_sender(
        &self,
    ) -> Result<Option<reviewq_providers::WindowsDesktop>, Failure> {
        self.windows
            .as_ref()
            .map(windows::Registration::sender)
            .transpose()
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    next: Release,
    staging: String,
    was_enabled: bool,
    was_running: bool,
    store_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<RecoveryApproval>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryApproval {
    release_id: String,
    store_id: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Inventory {
    active: Option<Release>,
    store_id: Option<String>,
    pending: Option<Activation>,
    known: Vec<String>,
    removing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    removal_stage: Option<String>,
    /// Set once a data deletion's checks have passed, until the data is gone. `store_id`
    /// names the store being deleted, and the record is deleted last. A refused deletion
    /// leaves it unset, so the data counts as kept.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    deleting_data: bool,
    /// The Windows notification identity an uninstall has yet to remove. The next uninstall
    /// removes it and the next install adopts it.
    #[serde(skip_serializing_if = "Option::is_none")]
    windows_left: Option<windows::Registration>,
}

impl Inventory {
    /// Whether the record holds nothing, so it can be deleted.
    fn is_empty(&self) -> bool {
        self.active.is_none()
            && self.store_id.is_none()
            && self.pending.is_none()
            && self.known.is_empty()
            && !self.removing
            && self.removal_stage.is_none()
            && !self.deleting_data
            && self.windows_left.is_none()
    }
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

pub(crate) fn verify_unit(paths: &Paths) -> Result<(), Failure> {
    let inventory = load(paths)?;
    let digest = files::unit_digest(&paths.unit)?.ok_or("ReviewQ has no installed user unit.")?;
    if inventory
        .active
        .iter()
        .chain(inventory.pending.iter().map(|step| &step.next))
        .any(|release| release.unit == digest)
    {
        return Ok(());
    }
    Err(paths::conflict(&paths.unit))
}

pub(crate) fn checkout_skill(profile: &Path) -> Result<Option<PathBuf>, Failure> {
    if !crate::runtime::profile(None)
        .as_ref()
        .is_ok_and(|installed| installed == profile)
    {
        return Ok(None);
    }
    let paths = Paths::resolve()?;
    let Some(release) = installed(&paths)? else {
        return Ok(None);
    };
    files::verify_current(&paths, &release)?;
    Ok(Some(
        profile
            .join("releases")
            .join(release.id)
            .join("hosts/shared/skills/reviewq-manage/SKILL.md"),
    ))
}

pub(crate) async fn install(bundle_path: &Path) -> Result<(), Failure> {
    let bundle = Bundle::read(bundle_path)?;
    let paths = Paths::resolve()?;
    let _lock = paths.lock()?;
    let mut inventory = load(&paths)?;
    if inventory.removing || inventory.deleting_data {
        uninstall::finish(&paths).await?;
        inventory = load(&paths)?;
        // Installing over a record still marked would let a later install delete the new data.
        if inventory.removing || inventory.deleting_data {
            return Err(format!(
                "ReviewQ could not finish the interrupted uninstall, so nothing was installed. The lines above say what is still in place. Remove it, or delete {}, then install again.",
                journal(&paths).display()
            )
            .into());
        }
    }
    let settings = runtime_settings::Settings::load(&paths.profile, false)?;
    // A Windows identity an uninstall could not remove is this installation's to reuse.
    let previous_windows = inventory
        .active
        .as_ref()
        .and_then(|release| release.windows.clone())
        .or_else(|| inventory.windows_left.clone());
    service::manager().await?;
    service::unit_location(&paths).await?;
    let unit = service::unit(
        &paths,
        &bundle.root.join("packaging/reviewq.service.in"),
        paths
            .claude_home_is_explicit
            .then_some(paths.claude_home.as_path()),
    )?;
    let codex = native_command(&paths, "codex", settings.codex_command.as_deref());
    let skill = codex
        .as_ref()
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
        windows: windows::Registration::plan(
            &bundle,
            inventory
                .pending
                .as_ref()
                .and_then(|operation| operation.next.windows.as_ref())
                .or(inventory
                    .active
                    .as_ref()
                    .and_then(|release| release.windows.as_ref()))
                // A left identity is adopted only where Windows can still be reached; it
                // otherwise stays recorded for the next uninstall.
                .or(inventory
                    .windows_left
                    .as_ref()
                    .filter(|_| reviewq_providers::WindowsDesktop::available())),
        )?,
    };
    if let Some(host) = &next.claude {
        claude::validate(&paths, &bundle, host).await?;
    }
    let old = inventory.active.as_ref();
    let staging = match inventory.pending.as_ref() {
        Some(pending) if pending.next == next => pending.staging.clone(),
        Some(_) => {
            return Err("Retry the exact pending bundle before selecting another release.".into());
        }
        None => files::reserve_stage(&paths)?,
    };
    let retention = retention::Plan::prepare(&paths, old, &bundle, &inventory.known, &staging)?;
    let pending = match inventory.pending.as_ref() {
        Some(pending) => pending,
        None => {
            files::preflight(&paths, old, None)?;
            start_door::check(&paths)?;
            bridge::check(&paths, old.is_some())?;
            if let Some(windows) = &next.windows {
                windows.preflight(previous_windows.as_ref()).await?;
            }
            files::check_new_skill(old, &next)?;
            claude::preflight(&paths, old, &next, false).await?;
            let (was_enabled, was_running) = service::state().await?;
            let store_id = if was_running {
                let previous = old.ok_or("Preserved an unrelated running user service.")?;
                Some(
                    service::ready(
                        &paths,
                        &previous.binary,
                        previous.store,
                        StartupCheck::Operational,
                    )
                    .await?
                    .store_id,
                )
            } else {
                probe(&paths, next.store).await?
            };
            if inventory.store_id.is_some() && inventory.store_id != store_id {
                // An installed release must keep the store it has been using. After an
                // uninstall nothing uses the retained store, so the install takes the data
                // folder as it is now and says so, and later checks follow that store.
                if old.is_some() {
                    return Err(
                        "The installed profile's retained store is missing or changed.".into(),
                    );
                }
                let folder = paths.profile.display();
                match &store_id {
                    Some(_) => println!(
                        "ReviewQ's data at {folder} changed while ReviewQ was uninstalled. Installing with the data that is there now."
                    ),
                    None => {
                        println!("ReviewQ's earlier data at {folder} is gone. Installing fresh.")
                    }
                }
                inventory.store_id.clone_from(&store_id);
            }
            paths.prepare()?;
            if !inventory.known.iter().any(|id| id == &next.id) {
                inventory.known.push(next.id.clone());
            }
            inventory.pending = Some(Activation {
                next: next.clone(),
                staging,
                was_enabled,
                was_running,
                store_id,
                recovery: None,
            });
            paths::write_json(&journal(&paths), &inventory)?;
            inventory
                .pending
                .as_ref()
                .ok_or("Activation was not recorded.")?
        }
    };
    let pending = pending.clone();
    let old = inventory.active.clone();
    let old = old.as_ref();
    files::preflight(&paths, old, Some(&next))?;
    let previous_unit = if let Some(previous) = old {
        files::read_unit(&paths.unit)?
            .filter(|unit| bundle::hex(&sha2::Sha256::digest(unit)) == previous.unit)
    } else {
        None
    };
    bridge::check(&paths, true)?;
    claude::preflight(&paths, old, &next, true).await?;
    retention.record(&paths)?;
    files::stage(&paths, &bundle, &pending.staging, &retention)?;
    let result = async {
        let identity = activate(
            &paths,
            old,
            previous_windows.as_ref(),
            &next,
            &unit,
            pending.store_id.as_deref(),
        )
        .await?;
        authorize_recovery(&paths, &next, &identity, &mut inventory).await
    }
    .await;
    let identity = match result {
        Ok(identity) => identity,
        Err(error) => {
            if let Some(previous) = old {
                rollback(&paths, previous, previous_unit, &pending, &mut inventory).await?;
                inventory.pending = None;
                paths::write_json(&journal(&paths), &inventory)?;
                if pending.was_running {
                    service::ready(
                        &paths,
                        &previous.binary,
                        previous.store,
                        StartupCheck::Operational,
                    )
                    .await?;
                }
                let preserved = retention.restore(&paths, previous, &next, &mut inventory.known)
                    .map_err(|cleanup| format!("{error} ReviewQ restored the previous release, but candidate cleanup was interrupted: {cleanup}"))?;
                paths::write_json(&journal(&paths), &inventory)?;
                report_preserved(preserved);
            }
            return Err(error);
        }
    };
    inventory.active = Some(next.clone());
    inventory.store_id = Some(identity.store_id.clone());
    inventory.pending = None;
    if next.windows.is_some() {
        inventory.windows_left = None;
    }
    paths::write_json(&journal(&paths), &inventory)?;
    let active =
        service::ready(&paths, &next.binary, next.store, StartupCheck::Operational).await?;
    if active.store_id != identity.store_id {
        return Err("A different store started after installation was committed.".into());
    }
    let endpoint =
        crate::runtime::read_endpoint_with_build(&paths.profile, &identity.build_digest)?;
    let preserved = retention
        .finish(&paths, &next, &mut inventory.known)
        .map_err(|error| {
            format!("ReviewQ is active, but release cleanup was interrupted: {error}")
        })?;
    paths::write_json(&journal(&paths), &inventory)?;
    for path in preserved {
        eprintln!(
            "Preserved a modified installation entry at {}.",
            path.display()
        );
    }
    // The hooks are optional and are installed only after the new release has passed
    // rollback and operational readiness. Their ownership survives an interrupted write,
    // independently of the active release.
    let codex_hooks = codex_hook::reconcile(&paths, next.skill.is_some());
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
    let hooks_installed = match (next.skill.is_some(), codex_hooks) {
        (has_codex, Ok(codex_hook::Reconciled::Done)) => has_codex,
        (has_codex, Ok(codex_hook::Reconciled::Kept { files, reason })) => {
            for file in files {
                eprintln!(
                    "ReviewQ could not remove a Codex hook of its own from {}: {reason} Remove the hook there that runs ReviewQ's launcher and leave the others.",
                    file.display()
                );
            }
            has_codex
        }
        (true, Err(error)) => {
            eprintln!(
                "ReviewQ could not configure its Codex hooks: {error} Reviews still file and close without them."
            );
            false
        }
        (false, Err(error)) => {
            eprintln!("ReviewQ preserved its previous Codex hooks: {error}");
            false
        }
    };
    if hooks_installed {
        let approval = match &codex {
            Some(executable) => approve_codex_hooks(&paths, executable).await,
            None => Err("Codex is unavailable.".into()),
        };
        match approval {
            Ok(HookApproval::Approved(count)) => println!(
                "ReviewQ approved {count} of its Codex hooks in {}. Review or turn them off on Codex's /hooks screen.",
                paths.codex_home.join("config.toml").display()
            ),
            Ok(HookApproval::AlreadyTrusted) => {}
            // Codex lists none of the entries just written when it will not run them.
            Ok(HookApproval::NotListed | HookApproval::Unsupported) | Err(_) => {
                if native_client::codex_stop_hook_unreported(&endpoint).await {
                    println!("{}", native_client::CODEX_HOOKS_PENDING);
                }
            }
        }
    }
    if next.windows.is_none() && reviewq_providers::WindowsDesktop::unreachable() {
        println!(
            "Windows unreachable from WSL, so ReviewQ set up no Windows desktop notifications. Retry install once /mnt/c is available."
        );
    }
    if next.claude.is_none() {
        println!("Claude Code unavailable. Set claudeCommand in runtime.json and retry install.");
    } else {
        println!("Use /reload-plugins in open Claude Code sessions to load this release.");
    }
    Ok(())
}

/// Asks the Codex at `executable` to trust the hooks this installation wrote.
async fn approve_codex_hooks(paths: &Paths, executable: &Path) -> Result<HookApproval, Failure> {
    let command = codex_hook::command(paths)?;
    Ok(reviewq_providers::approve_codex_hooks(executable, &paths.codex_home, &command).await?)
}

/// The command of the Codex hooks this installation writes.
pub(crate) fn codex_hook_command(paths: &Paths) -> Option<String> {
    codex_hook::command(paths).ok()
}

fn report_preserved(entries: Vec<PathBuf>) {
    for path in entries {
        eprintln!(
            "Preserved a modified installation entry at {}.",
            path.display()
        );
    }
}

async fn authorize_recovery(
    paths: &Paths,
    release: &Release,
    identity: &reviewq_transport::ServiceIdentity,
    inventory: &mut Inventory,
) -> Result<reviewq_transport::ServiceIdentity, Failure> {
    let pending = inventory
        .pending
        .as_mut()
        .ok_or("The activation journal is missing.")?;
    pending.recovery = Some(RecoveryApproval {
        release_id: release.id.clone(),
        store_id: identity.store_id.clone(),
    });
    paths::write_json(&journal(paths), inventory)?;
    let recovered = service::ready(
        paths,
        &release.binary,
        release.store,
        StartupCheck::Recovered,
    )
    .await?;
    if recovered.store_id != identity.store_id {
        return Err("The service completed recovery against a different store.".into());
    }
    Ok(recovered)
}

async fn activate(
    paths: &Paths,
    old: Option<&Release>,
    previous_windows: Option<&windows::Registration>,
    next: &Release,
    unit: &[u8],
    store_id: Option<&str>,
) -> Result<reviewq_transport::ServiceIdentity, Failure> {
    start_door::stop().await?;
    if service::state().await?.1 {
        service::switch("stop").await?;
    }
    let stopped_store = probe(paths, next.store).await?;
    if store_id.is_some_and(|id| stopped_store.as_deref() != Some(id)) {
        return Err("The retained data changed during activation.".into());
    }
    files::set(paths, old, next, unit)?;
    start_door::write(paths)?;
    claude::activate(paths, old, next).await?;
    if let Some(windows) = &next.windows {
        let release = paths.profile.join("releases").join(&next.id);
        windows.set(&release, previous_windows).await?;
    }
    service::reload().await?;
    service::unit_location(paths).await?;
    start_door::open().await;
    // Only a first install turns on starting at login; an update keeps the user's choice.
    if old.is_none() {
        service::switch("enable").await?;
    }
    service::switch("start").await?;
    let identity = service::ready(paths, &next.binary, next.store, StartupCheck::Identity).await?;
    if stopped_store.is_some_and(|id| id != identity.store_id) {
        return Err("The started service did not retain the verified store.".into());
    }
    Ok(identity)
}

async fn rollback(
    paths: &Paths,
    previous: &Release,
    previous_unit: Option<Vec<u8>>,
    pending: &Activation,
    inventory: &mut Inventory,
) -> Result<(), Failure> {
    files::preflight(paths, Some(previous), Some(&pending.next))?;
    let bundle = Bundle::inventory(&paths.profile.join("releases").join(&previous.id))?;
    bundle.verify()?;
    if bundle.id != previous.id || bundle.manifest.store != previous.store {
        return Err("The previous release could not be verified.".into());
    }
    let unit = match previous_unit {
        Some(unit) => unit,
        None => verified_unit(paths, previous, &bundle)?,
    };
    // The door goes with the release that wrote it. The previous release may be from before
    // the door, with no `start-door` command and nothing that would ever remove it, and no
    // record could say so: an older release reads the install record with unknown fields
    // refused. A release that has the door puts it back once its service is up.
    start_door::retire().await?;
    if service::state().await?.1 {
        service::switch("stop").await?;
    }
    let expected_store = pending
        .store_id
        .as_deref()
        .ok_or("The previous installation has no verified store identity.")?;
    if probe(paths, previous.store).await?.as_deref() != Some(expected_store) {
        return Err("The previous installation's retained store is missing or changed.".into());
    }
    files::set(paths, Some(&pending.next), previous, &unit)?;
    start_door::remove(paths)?;
    if previous.claude.is_some() {
        let previous_paths = Paths {
            claude_home_is_explicit: service::unit_has_claude_override(&unit),
            ..paths.clone()
        };
        claude::activate(&previous_paths, Some(&pending.next), previous).await?;
    } else {
        claude::remove(paths, &pending.next).await?;
    }
    if let Some(windows) = &previous.windows {
        windows
            .set(&bundle.root, pending.next.windows.as_ref())
            .await?;
    } else if let Some(windows) = &pending.next.windows {
        windows.remove().await?;
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
        let identity = service::ready(
            paths,
            &previous.binary,
            previous.store,
            StartupCheck::Identity,
        )
        .await?;
        if identity.store_id != expected_store {
            return Err("The previous service opened a different store.".into());
        }
        authorize_recovery(paths, previous, &identity, inventory).await?;
    }
    Ok(())
}

fn verified_unit(paths: &Paths, release: &Release, bundle: &Bundle) -> Result<Vec<u8>, Failure> {
    let template = bundle.root.join("packaging/reviewq.service.in");
    let claude_home = release
        .claude
        .as_ref()
        .map_or(paths.claude_home.as_path(), |host| host.home.as_path());
    // Earlier releases rendered the default Claude directory as an override.
    for config_dir in [Some(claude_home), None] {
        let unit = service::unit(paths, &template, config_dir)?;
        if bundle::hex(&sha2::Sha256::digest(&unit)) == release.unit {
            return Ok(unit);
        }
    }
    Err("The previous service unit no longer matches its installation.".into())
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
