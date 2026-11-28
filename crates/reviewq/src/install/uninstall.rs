//! Uninstalling ReviewQ: the checks made before anything changes, shared by the CLI and the
//! dashboard; the transient job the dashboard hands the work to, since the service cannot
//! remove itself; the removal; with `--delete-data`, deleting the owned data once everything
//! else is gone; then the Windows notification identity; and, last of a data deletion that
//! removed the record, the installation lock's file.

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
    probe, refused, retention, start_door,
};
use crate::{runtime_settings, service};

/// What `reviewq uninstall` was asked to do.
pub(crate) struct Options {
    /// Delete the owned data once every removal step has succeeded.
    pub delete_data: bool,
    /// Set by the dashboard's job: refuse unless the install record names this store, and
    /// send a desktop notification when the uninstall fails or leaves anything in place.
    pub expect_store: Option<String>,
}

/// The checks an uninstall makes before it changes anything: no activation is pending, and
/// while ReviewQ is installed its service unit on disk is the installed one and its start
/// door files are ReviewQ's. Returns the install journal. Reads whether the unit overrides Claude's home into
/// `paths`.
async fn preflight(paths: &mut Paths) -> Result<Inventory, Failure> {
    let inventory = load(paths)?;
    if inventory.pending.is_some() {
        return Err(refused(
            "Finish the interrupted activation before uninstalling.",
        ));
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
        start_door::check(paths)?;
    }
    Ok(inventory)
}

/// The dashboard's uninstall. Makes the uninstall's checks, then starts `reviewq uninstall`
/// as the transient user service `reviewq-uninstall`, outside `reviewq.service`, which it
/// stops. The caller holds the installation lock and releases it once this returns, and
/// the job then waits for it. A `Refusal` is the sentence the page shows; any other failure
/// is internal.
pub(crate) async fn start_job(paths: &Paths, delete_data: bool) -> Result<(), Failure> {
    let mut paths = paths.clone();
    let inventory = preflight(&mut paths).await?;
    let store = inventory
        .store_id
        .filter(|_| inventory.active.is_some())
        .ok_or_else(|| refused("ReviewQ is not installed for this profile."))?;
    let job = format!("{}.service", service::UNINSTALL_JOB);
    if service::unit_state(&job).await?.1 {
        return Err(refused("An uninstall is already running."));
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
        return Err(refused("An uninstall is already running."));
    }
    Err(refused(
        "ReviewQ couldn't start its uninstall. Run `reviewq uninstall` in a terminal.",
    ))
}

fn setenv(key: &str, value: &Path) -> OsString {
    let mut argument = OsString::from(format!("--setenv={key}="));
    argument.push(value);
    argument
}

/// Something an uninstall left in place, already reported with the reason.
struct Left {
    name: String,
    /// Removing it failed, rather than a rule keeping it, such as a link or a changed entry.
    failed: bool,
}

/// `reviewq uninstall`. Lists anything left in place at the end, and fails when removing any
/// of it failed; entries kept by rule leave nothing to retry. The dashboard's job also says
/// so in one desktop notification, as it does for a failure.
pub(crate) async fn uninstall(options: Options) -> Result<(), Failure> {
    let mut paths = Paths::resolve()?;
    let lock = paths.lock()?;
    let job = options.expect_store.is_some();
    // A data deletion that stopped part way is finished even when not asked again.
    let delete_data = options.delete_data || load(&paths).is_ok_and(|record| record.deleting_data);
    let mut left = match remove(&mut paths, &options).await {
        Ok(left) => left,
        Err(error) => {
            let failure = failed(&paths, delete_data, error, job).await;
            retire_windows_if_removed(&paths).await;
            return failure;
        }
    };
    // Sent while the Windows identity is still registered, so it shows on Windows.
    let noticed = job && !left.is_empty();
    if noticed {
        notify(&paths, &left_in_place(&left, true)).await;
    }
    match retire_windows(&paths, delete_data).await {
        Ok(more) => {
            if job && !noticed && !more.is_empty() {
                notify(&paths, &left_in_place(&more, true)).await;
            }
            left.extend(more);
        }
        Err(error) => return failed(&paths, delete_data, error, job).await,
    }
    // Last of all, so nothing that waited for the lock runs while this uninstall still works.
    // `reviewq install` finishes an uninstall under a lock it keeps, so only this deletes it.
    if delete_data && !paths::exists(&journal(&paths))? {
        left.extend(report(data::Folders::installed(&paths).delete_lock(lock)));
    }
    if left.is_empty() {
        return Ok(());
    }
    let sentence = left_in_place(&left, false);
    if left.iter().any(|entry| entry.failed) {
        return Err(sentence.into());
    }
    println!("{sentence}");
    Ok(())
}

/// Finishes an uninstall that stopped part way, for `reviewq install`, which holds the
/// installation lock and then installs into what it leaves: the removal, keeping the data,
/// or the data deletion that was under way. What it leaves in place is reported and does not
/// stop the install, which adopts a Windows identity left behind.
pub(super) async fn finish(paths: &Paths) -> Result<(), Failure> {
    let mut paths = paths.clone();
    let deleting_data = load(&paths)?.deleting_data;
    println!("Finishing the interrupted uninstall first.");
    let options = Options {
        delete_data: deleting_data,
        expect_store: None,
    };
    remove(&mut paths, &options).await?;
    Ok(())
}

/// Removes everything but the Windows identity, under the caller's installation lock, and
/// returns what it left in place, each already reported with the reason. Resumes an
/// interrupted removal or data deletion.
async fn remove(paths: &mut Paths, options: &Options) -> Result<Vec<Left>, Failure> {
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
        let mut left = by_rule(&retire_codex_hooks(paths).await?);
        if options.delete_data || inventory.deleting_data {
            left.extend(delete_data(paths, inventory.store_id.as_deref())?);
        } else if left.is_empty() {
            if paths::exists(&paths.profile.join("reviewq.sqlite3"))? {
                println!(
                    "ReviewQ is already removed. Retained data {}",
                    paths.profile.display()
                );
            } else {
                println!(
                    "ReviewQ is already removed. It has no data at {}.",
                    paths.profile.display()
                );
            }
        }
        return Ok(left);
    };
    // Nothing may start the service while it is being removed.
    start_door::retire().await?;
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
        start_door::remove(paths)?;
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
    // The launcher and releases are gone, so a kept hook entry no longer holds the uninstall
    // open: it is reported once the removal is recorded.
    retention::clear_assets_journal(paths)?;
    // The Windows identity stays registered until last, so every notice before then still
    // shows on Windows. With `--delete-data` the record keeps the store's ID, so the deletion
    // checks it is deleting that store; the deletion itself marks the record once its checks
    // pass, so a refused deletion leaves a record that keeps the data.
    let store_id = inventory.store_id.take();
    inventory = Inventory {
        store_id: store_id.clone().filter(|_| options.delete_data),
        windows_left: active.windows.clone().or(inventory.windows_left.take()),
        ..Inventory::default()
    };
    paths::write_json(&journal(paths), &inventory)?;
    let mut left = by_rule(&kept_hooks);
    if options.delete_data {
        println!("ReviewQ removed.");
        left.extend(delete_data(paths, store_id.as_deref())?);
    } else {
        println!(
            "ReviewQ removed. Retained data at {} and runtime settings at {}.",
            paths.profile.display(),
            runtime_settings::config_path(&paths.profile, false).display()
        );
    }
    Ok(left)
}

/// Deletes the owned data after ReviewQ is removed, and returns what it kept. Refuses while
/// ReviewQ's unit or launcher is still in place, since then it belongs to an installation
/// this record does not name. Holds the store's own lock first, which proves nothing still
/// has the data open, and refuses a store other than `expected`. Only then marks the record
/// as deleting, so an interrupted deletion resumes and a refused one keeps the data. Deletes
/// the record last; while a Windows identity is left, the record keeps only that. A refusal
/// of a deletion an earlier attempt began says that nothing more was deleted.
fn delete_data(paths: &Paths, expected: Option<&str>) -> Result<Vec<Left>, Failure> {
    let mut record = load(paths)?;
    let nothing = if record.deleting_data {
        "nothing more was deleted"
    } else {
        "nothing was deleted"
    };
    for installed in [&paths.unit, &paths.launcher] {
        if paths::exists(installed)? {
            return Err(refused(format!(
                "ReviewQ still looks installed, since {} exists, but {} records no installation, so {nothing}. Run `reviewq uninstall --delete-data` with the XDG_STATE_HOME ReviewQ was installed with.",
                installed.display(),
                journal(paths).display()
            )));
        }
    }
    let held = if paths::exists(&paths.profile)? {
        let database = AbsolutePath::from_canonical(paths.profile.join("reviewq.sqlite3"))?;
        // ReviewQ is already removed here, so no `reviewq` command is left to run again.
        let held = SqliteStore::hold_stopped(&database)?.ok_or_else(|| {
            refused(format!(
                "Another ReviewQ process, such as `reviewq serve`, still has the data open, so {nothing}. Stop it, then delete {} yourself.",
                folders(paths)
            ))
        })?;
        if let (Some(expected), Some(identity)) = (expected, &held.identity)
            && identity.store_id.to_string() != expected
        {
            return Err(refused(format!(
                "The data folder holds a different store than the one ReviewQ installed, so {nothing}. If you no longer need it, delete {} yourself.",
                folders(paths)
            )));
        }
        Some(held)
    } else {
        None
    };
    record.deleting_data = true;
    paths::write_json(&journal(paths), &record)?;
    let windows_left = record.windows_left;
    let deletion = data::Folders::installed(paths).delete(windows_left.is_some())?;
    drop(held);
    if windows_left.is_some() {
        paths::write_json(
            &journal(paths),
            &Inventory {
                windows_left,
                ..Inventory::default()
            },
        )?;
    }
    Ok(report(deletion))
}

/// ReviewQ's own folders, for a sentence that tells the user to delete them by hand.
fn folders(paths: &Paths) -> String {
    format!(
        "{}, {} and {}",
        paths.profile.display(),
        paths.config.display(),
        paths.state.display()
    )
}

/// Prints a deletion and returns what it kept.
fn report(deletion: data::Deletion) -> Vec<Left> {
    for path in &deletion.deleted {
        println!("Deleted {}", path.display());
    }
    deletion
        .kept
        .into_iter()
        .map(|(path, kept)| {
            println!("Kept {} because {}.", path.display(), kept.reason());
            Left {
                name: path.display().to_string(),
                failed: kept.failed(),
            }
        })
        .collect()
}

/// Removes the Windows notification identity an uninstall left registered, last of all, and
/// returns what it left. When Windows cannot remove it, the record keeps it, so the next
/// install reuses it and an uninstall run again removes it. Otherwise a record left with
/// nothing in it is deleted, which `--delete-data` reports with the folders it leaves empty.
async fn retire_windows(paths: &Paths, delete_data: bool) -> Result<Vec<Left>, Failure> {
    let mut inventory = load(paths)?;
    let Some(windows) = inventory.windows_left.take() else {
        return Ok(Vec::new());
    };
    if let Err(error) = windows.remove().await {
        report_identity_kept(&error);
        return Ok(vec![Left {
            name: "the Windows notification identity Personal.ReviewQ".into(),
            failed: true,
        }]);
    }
    if !inventory.is_empty() {
        paths::write_json(&journal(paths), &inventory)?;
        return Ok(Vec::new());
    }
    if delete_data {
        return Ok(report(data::Folders::installed(paths).delete_record()));
    }
    std::fs::remove_file(journal(paths))?;
    paths::sync(&paths.state)?;
    Ok(Vec::new())
}

/// After a failure, removes the Windows notification identity when ReviewQ itself is already
/// gone, as a refused data deletion leaves it: no launcher is left to run `reviewq uninstall`
/// again, and the refusal tells the user to delete the folders by hand. Runs after the failure
/// notice, which needs the identity. Only the record's left identity is touched, as a
/// keep-data uninstall does, and only while neither the unit nor the launcher exists and the
/// record names no installation, activation or removal in progress: another installation's
/// identity, and one an interrupted first install still owns, stay. A failure here is
/// reported and the caller's failure is returned unchanged.
async fn retire_windows_if_removed(paths: &Paths) {
    let removed = !paths::exists(&paths.unit).unwrap_or(true)
        && !paths::exists(&paths.launcher).unwrap_or(true)
        && load(paths).is_ok_and(|record| {
            record.active.is_none() && record.pending.is_none() && !record.removing
        });
    if !removed {
        return;
    }
    if let Err(error) = retire_windows(paths, false).await {
        report_identity_kept(&error);
    }
}

/// Says that the Windows notification identity is still registered and recorded, and what
/// takes it from there. By then ReviewQ's launcher is gone, so a retry runs the program in a
/// release folder, which needs nothing but the install record.
fn report_identity_kept(error: &Failure) {
    eprintln!(
        "ReviewQ could not finish removing its Windows notification identity Personal.ReviewQ: {error} The next ReviewQ install reuses it. To try the removal again, run `bin/reviewq uninstall` from a ReviewQ release folder."
    );
}

/// Reports `error`, in a desktop notification for the dashboard's job, and returns it.
async fn failed(
    paths: &Paths,
    delete_data: bool,
    error: Failure,
    job: bool,
) -> Result<(), Failure> {
    if job {
        let next = if !paths::exists(&paths.launcher).unwrap_or(false) {
            "`journalctl --user -u reviewq-uninstall` has the details."
        } else if delete_data {
            "Run `reviewq uninstall --delete-data` to finish."
        } else {
            "Run `reviewq uninstall` to finish."
        };
        let body = format!(
            "ReviewQ couldn't finish uninstalling: {}. {next}",
            error.to_string().trim_end_matches('.')
        );
        notify(paths, &body).await;
    }
    Err(error)
}

/// The sentence for an uninstall that finished but left `left` in place. The notification
/// names at most three and points to the job's journal for the reasons.
fn left_in_place(left: &[Left], notice: bool) -> String {
    let kept = left
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    if !notice {
        return format!(
            "ReviewQ is uninstalled, but left in place: {}",
            kept.join(", ")
        );
    }
    let mut named = kept[..kept.len().min(3)].join(", ");
    if kept.len() > 3 {
        named.push_str(&format!(" and {} more", kept.len() - 3));
    }
    format!(
        "ReviewQ is uninstalled, but left in place: {named}. `journalctl --user -u reviewq-uninstall` says why."
    )
}

/// Entries a rule kept, such as hook entries the user changed.
fn by_rule(paths: &[PathBuf]) -> Vec<Left> {
    names(paths)
        .into_iter()
        .map(|name| Left {
            name,
            failed: false,
        })
        .collect()
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
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
        names(entries).join(", ")
    )
    .into()
}

/// Shows `body` on the desktop: through the Windows sender while ReviewQ's identity is still
/// registered, otherwise through `notify-send`. The journal of `reviewq-uninstall` always has
/// the full reason.
async fn notify(paths: &Paths, body: &str) {
    let port = runtime_settings::Settings::load(&paths.profile, false)
        .map(|settings| settings.port)
        .unwrap_or_else(|_| runtime_settings::Settings::default().port);
    let windows = load(paths)
        .ok()
        .and_then(|inventory| {
            inventory
                .active
                .and_then(|release| release.windows)
                .or(inventory.windows_left)
        })
        .and_then(|registration| registration.sender().ok());
    if let Some(sender) = windows {
        let browser = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        if matches!(
            sender.show(browser, "ReviewQ", body, "uninstall").await,
            DesktopOutcome::Delivered
        ) {
            return;
        }
    }
    if let Some(binary) = runtime_settings::executable("notify-send")
        && let Some(binary) = binary.to_str()
    {
        let _ =
            service::capture(binary, &["--app-name=ReviewQ", "--", "ReviewQ", body], true).await;
    }
}
