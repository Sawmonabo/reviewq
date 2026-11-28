//! Uninstalling ReviewQ: the checks made before anything changes, shared by the CLI and the
//! dashboard; the transient job the dashboard hands the work to, since the service cannot
//! remove itself; the removal; and, with `--delete-data`, deleting the owned data once
//! everything else is gone.

use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use reviewq_app::notifications::DesktopOutcome;
use reviewq_domain::session::AbsolutePath;
use reviewq_providers::HookRevocation;
use reviewq_store::SqliteStore;
use sha2::Digest;
use tokio::process::Command;

use super::{
    Failure, Inventory, bundle, claude, codex_hook, data, files, journal, load, native_command,
    paths::{self, Paths},
    probe, retention, start_socket,
};
use crate::{runtime_settings, service};

/// What `reviewq uninstall` was asked to do.
pub(crate) struct Options {
    /// Delete the owned data once every removal step has succeeded.
    pub delete_data: bool,
    /// Set by the dashboard's job: refuse unless the install record names this store, and
    /// send a desktop notification when the uninstall fails.
    pub expect_store: Option<String>,
}

/// The checks an uninstall makes before it changes anything: no activation is pending, and
/// while ReviewQ is installed its service unit and start socket on disk are the installed
/// ones. Returns the install journal. Reads whether the unit overrides Claude's home into
/// `paths`.
async fn preflight(paths: &mut Paths) -> Result<Inventory, Failure> {
    let inventory = load(paths)?;
    if inventory.pending.is_some() {
        return Err("Finish the interrupted activation before uninstalling.".into());
    }
    if let Some(active) = &inventory.active {
        service::manager().await?;
        service::unit_location(paths).await?;
        if let Some(unit) = files::read_unit(&paths.unit)? {
            if bundle::hex(&sha2::Sha256::digest(&unit)) != active.unit {
                return Err(paths::conflict(&paths.unit));
            }
            paths.claude_home_is_explicit = service::unit_has_claude_override(&unit);
        }
        start_socket::check(paths)?;
    }
    Ok(inventory)
}

/// The dashboard's uninstall. Makes the uninstall's checks, then starts `reviewq uninstall`
/// as the transient user service `reviewq-uninstall`, outside `reviewq.service`, which it
/// stops. The caller holds the installation lock and releases it once this returns, and
/// the job then waits for it. Each failure is the sentence the page shows.
pub(crate) async fn start_job(paths: &Paths, delete_data: bool) -> Result<(), Failure> {
    let mut paths = paths.clone();
    let inventory = preflight(&mut paths).await?;
    let store = inventory
        .store_id
        .filter(|_| inventory.active.is_some())
        .ok_or("ReviewQ is not installed for this profile.")?;
    let job = format!("{}.service", service::UNINSTALL_JOB);
    if service::unit_state(&job).await?.1 {
        return Err("An uninstall is already running.".into());
    }
    let data_home = paths::parent(&paths.profile)?;
    let mut command = Command::new("systemd-run");
    command.args([
        "--user",
        "--unit",
        service::UNINSTALL_JOB,
        "--collect",
        "--quiet",
        "--service-type=exec",
        "--property=UMask=0077",
    ]);
    // Exactly the variables the installed unit sets.
    for (key, value) in [
        ("HOME", paths.home.as_path()),
        ("XDG_DATA_HOME", data_home),
        ("XDG_CONFIG_HOME", &paths.config_home),
        ("XDG_STATE_HOME", &paths.state_home),
        ("CODEX_HOME", &paths.codex_home),
    ] {
        command.arg(setenv(key, value));
    }
    if paths.claude_home_is_explicit {
        command.arg(setenv("CLAUDE_CONFIG_DIR", &paths.claude_home));
    } else {
        command.arg("--property=UnsetEnvironment=CLAUDE_CONFIG_DIR");
    }
    // The release's own binary: uninstall removes the `current` link, and Linux keeps a
    // running binary usable after its file is deleted.
    command
        .arg("--")
        .arg(std::env::current_exe()?)
        .args(["uninstall", "--expect-store", &store]);
    if delete_data {
        command.arg("--delete-data");
    }
    if service::output(command, true).await?.0 {
        return Ok(());
    }
    if service::unit_state(&job).await?.1 {
        return Err("An uninstall is already running.".into());
    }
    Err("ReviewQ couldn't start its uninstall. Run `reviewq uninstall` in a terminal.".into())
}

fn setenv(key: &str, value: &Path) -> OsString {
    let mut argument = OsString::from(format!("--setenv={key}="));
    argument.push(value);
    argument
}

/// `reviewq uninstall`. A failed dashboard job also says so in a desktop notification.
pub(crate) async fn uninstall(options: Options) -> Result<(), Failure> {
    let mut paths = Paths::resolve()?;
    let result = remove(&mut paths, &options).await;
    if let Err(error) = &result
        && options.expect_store.is_some()
    {
        notify_failure(&paths, &error.to_string()).await;
    }
    result
}

async fn remove(paths: &mut Paths, options: &Options) -> Result<(), Failure> {
    let _lock = paths.lock()?;
    let mut inventory = preflight(paths).await?;
    if let Some(expected) = &options.expect_store
        && inventory.store_id.as_deref() != Some(expected)
    {
        return Err(
            "The install record here names a different store than the dashboard that started this uninstall, so nothing was removed."
                .into(),
        );
    }
    let Some(active) = inventory.active.clone() else {
        let kept_hooks = retire_codex_hooks(paths).await?;
        if options.delete_data {
            return delete_data(paths, inventory.store_id.as_deref());
        }
        if !kept_hooks.is_empty() {
            return Err(preserved_entries(&kept_hooks));
        }
        println!(
            "ReviewQ is already removed. Retained data {}",
            paths.profile.display()
        );
        return Ok(());
    };
    // Nothing may start the service while it is being removed.
    start_socket::retire(paths).await?;
    if inventory.removal_stage.is_none() {
        inventory.removal_stage = Some(files::reserve_removal(paths)?);
        inventory.removing = true;
        paths::write_json(&journal(paths), &inventory)?;
    }
    if service::state().await?.1 {
        service::switch("stop").await?;
    }
    if service::state().await?.0 {
        service::switch("disable").await?;
    }
    let expected = inventory
        .store_id
        .clone()
        .ok_or("Missing installed store identity.")?;
    if probe(paths, active.store).await?.as_deref() != Some(expected.as_str()) {
        return Err("The installed profile's retained store is missing or changed.".into());
    }
    let database = AbsolutePath::from_canonical(paths.profile.join("reviewq.sqlite3"))?;
    let store = SqliteStore::open(database).await?;
    if store.identity().store_id.to_string() != expected {
        return Err("ReviewQ opened a different store while uninstalling.".into());
    }
    let removed = crate::checkout_setup::remove_installation(&paths.profile, &store).await;
    store.close(std::time::Duration::from_secs(10)).await?;
    removed?;
    let kept_hooks = retire_codex_hooks(paths).await?;
    claude::remove(paths, &active).await?;
    super::bridge::remove(paths)?;
    let removal = inventory
        .removal_stage
        .as_deref()
        .ok_or("Missing removal stage.")?;
    let mut conflicts = retention::remove_assets(paths, &inventory.known)?;
    if conflicts.is_empty() {
        conflicts = files::remove(paths, &active, &inventory.known, removal)?;
    }
    if conflicts.is_empty() {
        start_socket::remove(paths)?;
    }
    service::reload().await?;
    if !conflicts.is_empty() {
        conflicts.extend(kept_hooks);
        let mut message = preserved_entries(&conflicts).to_string();
        if options.delete_data {
            message.push_str(". Nothing was deleted.");
        }
        return Err(message.into());
    }
    // Last, so the Windows notification sender still works for any earlier failure.
    if let Some(windows) = &active.windows {
        windows.remove().await?;
    }
    // The launcher and releases are gone, so a kept hook entry no longer holds the uninstall
    // open: it is reported once the removal is recorded.
    retention::clear_assets_journal(paths)?;
    // With `--delete-data` the record keeps the store's ID until the data is gone, so an
    // interrupted deletion can resume against the same store.
    let store_id = inventory.store_id.take();
    inventory = Inventory {
        store_id: store_id.clone().filter(|_| options.delete_data),
        ..Inventory::default()
    };
    paths::write_json(&journal(paths), &inventory)?;
    if options.delete_data {
        println!("ReviewQ removed.");
        return delete_data(paths, store_id.as_deref());
    }
    println!(
        "ReviewQ removed. Retained data at {} and runtime settings at {}.",
        paths.profile.display(),
        runtime_settings::config_path(&paths.profile, false).display()
    );
    if !kept_hooks.is_empty() {
        return Err(preserved_entries(&kept_hooks));
    }
    Ok(())
}

/// Deletes the owned data after ReviewQ is removed. Holds the store's own lock first, which
/// proves nothing still has the database open, and refuses a store other than `expected`.
/// A kept Codex hook was already reported and does not stop the deletion.
fn delete_data(paths: &Paths, expected: Option<&str>) -> Result<(), Failure> {
    let held = if paths::exists(&paths.profile.join("reviewq.sqlite3"))? {
        let database = AbsolutePath::from_canonical(paths.profile.join("reviewq.sqlite3"))?;
        let held = SqliteStore::hold_stopped(&database)?.ok_or(
            "Another ReviewQ process, such as `reviewq serve`, still has the data open, so nothing was deleted. Stop it and run `reviewq uninstall --delete-data` again.",
        )?;
        if let (Some(expected), Some(identity)) = (expected, &held.identity)
            && identity.store_id.to_string() != expected
        {
            return Err(
                "The data folder holds a different store than the one ReviewQ installed, so nothing was deleted."
                    .into(),
            );
        }
        Some(held)
    } else {
        None
    };
    let deletion = data::Folders::installed(paths).delete()?;
    drop(held);
    for path in &deletion.deleted {
        println!("Deleted {}", path.display());
    }
    for (path, reason) in &deletion.kept {
        println!("Kept {} because {reason}.", path.display());
    }
    Ok(())
}

/// Asks the Codex the runtime settings name to remove its trust in this installation's hooks.
async fn revoke_codex_hooks(paths: &Paths) -> Result<HookRevocation, Failure> {
    let configured = runtime_settings::Settings::load(&paths.profile, false)
        .ok()
        .and_then(|settings| settings.codex_command);
    let executable =
        native_command(paths, "codex", configured.as_deref()).ok_or("Codex is unavailable")?;
    let command = codex_hook::command(paths)?;
    Ok(reviewq_providers::revoke_codex_hooks(&executable, &paths.codex_home, &command).await?)
}

/// Removes ReviewQ's Codex hooks while uninstalling and returns the `hooks.json` files that
/// still hold an entry it could not remove, usually because the user changed it, after
/// saying what to remove there. Codex's trust in them is removed first, while Codex still
/// lists them; when that fails, one line says so. The uninstall continues. Fails when the
/// ownership file cannot be written or removed.
async fn retire_codex_hooks(paths: &Paths) -> Result<Vec<PathBuf>, Failure> {
    if paths::exists(&paths.codex_home.join("hooks.json"))? {
        let config = paths.codex_home.join("config.toml");
        match revoke_codex_hooks(paths).await {
            Ok(HookRevocation::Revoked(count)) => println!(
                "ReviewQ removed Codex's approval of {count} of its hooks from {}.",
                config.display()
            ),
            Ok(HookRevocation::NoneTrusted | HookRevocation::NotListed) => {}
            Ok(HookRevocation::Unsupported) => eprintln!(
                "This Codex cannot say which hooks it trusts, so any approval of ReviewQ's hooks stays in {}.",
                config.display()
            ),
            Err(error) => eprintln!(
                "ReviewQ could not remove Codex's approval of its hooks from {} ({error}).",
                config.display()
            ),
        }
    }
    match codex_hook::reconcile(paths, false)? {
        codex_hook::Reconciled::Done => Ok(Vec::new()),
        codex_hook::Reconciled::Kept { files, reason } => {
            for file in &files {
                eprintln!(
                    "ReviewQ could not remove a Codex hook of its own from {}: {reason} Remove the hook there that runs ReviewQ's launcher and leave the others.",
                    file.display()
                );
            }
            Ok(files)
        }
    }
}

/// The uninstall's failure when `entries` were left in place for the user to resolve.
fn preserved_entries(entries: &[PathBuf]) -> Failure {
    format!(
        "Preserved modified installation entries: {}",
        entries
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
    .into()
}

/// Tells the desktop that the dashboard's uninstall stopped, with the one step left. Uses
/// the Windows sender while it is still registered, otherwise `notify-send`. The journal of
/// `reviewq-uninstall` always has the full reason.
async fn notify_failure(paths: &Paths, reason: &str) {
    let next = if paths::exists(&paths.launcher).unwrap_or(false) {
        "Run `reviewq uninstall` to finish."
    } else {
        "`journalctl --user -u reviewq-uninstall` has the details."
    };
    let body = format!(
        "ReviewQ couldn't finish uninstalling: {}. {next}",
        reason.trim_end_matches('.')
    );
    let port = runtime_settings::Settings::load(&paths.profile, false)
        .map(|settings| settings.port)
        .unwrap_or_else(|_| runtime_settings::Settings::default().port);
    let windows = load(paths)
        .ok()
        .and_then(|inventory| inventory.active)
        .and_then(|release| release.windows_sender().ok().flatten());
    if let Some(sender) = windows {
        let browser = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        if matches!(
            sender.show(browser, "ReviewQ", &body, "uninstall").await,
            DesktopOutcome::Delivered
        ) {
            return;
        }
    }
    if let Some(binary) = runtime_settings::executable("notify-send")
        && let Some(binary) = binary.to_str()
    {
        let _ = service::capture(
            binary,
            &["--app-name=ReviewQ", "--", "ReviewQ", &body],
            true,
        )
        .await;
    }
}
