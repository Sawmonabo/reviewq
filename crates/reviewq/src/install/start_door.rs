//! The start door: `reviewq-start.socket` holds the start port while ReviewQ is stopped, and
//! each connection to it runs one `reviewq start-door` from the template
//! `reviewq-start@.service`. Both files are rendered here and handled together. Neither has an
//! install record entry: a file is ReviewQ's when its first line is [`MARKER`].

use std::fs;
use std::path::Path;
use std::time::Duration;

use super::{
    Failure, files,
    paths::{self, Paths},
};
use crate::{runtime_settings, service};

/// The first line of both door files, which marks them as ReviewQ's. Its text must never
/// change: every earlier install's files would then look like someone else's.
const MARKER: &str = "# ReviewQ start door. ReviewQ rewrites this file; edits are lost.";

/// Each door file's path with the bytes the current `runtime.json` and profile want there.
fn wanted(paths: &Paths) -> Result<[(&Path, Vec<u8>); 2], Failure> {
    let port = runtime_settings::Settings::load(&paths.profile, false)?.port;
    let start = runtime_settings::start_port(port)
        .ok_or("The browser port leaves no room for the start port.")?;
    // With no trigger limit, a burst of connections cannot put the socket in a failed state,
    // where it stops listening; older systemd has no poll limit to slow a burst first.
    // `MaxConnections`, 64 by default, still bounds how many door processes run at once.
    let socket = format!(
        "{MARKER}\n[Unit]\nDescription=ReviewQ start door\n\n[Socket]\nListenStream=127.0.0.1:{start}\nAccept=yes\nTriggerLimitIntervalSec=0\n\n[Install]\nWantedBy=sockets.target\n"
    );
    // The reply goes out over standard input, the connection, so output cannot corrupt it.
    let door = format!(
        "{MARKER}\n[Unit]\nDescription=ReviewQ start door connection\nCollectMode=inactive-or-failed\n\n[Service]\nType=exec\nExecStart=:{} start-door\nStandardInput=socket\nStandardOutput=journal\nStandardError=journal\nRuntimeMaxSec=10\nUMask=0077\n",
        service::executable(paths)?
    );
    Ok([
        (paths.start_socket.as_path(), socket.into_bytes()),
        (paths.start_door.as_path(), door.into_bytes()),
    ])
}

/// Whether `unit` is ReviewQ's: its first line is exactly the marker.
fn owned(unit: &[u8]) -> bool {
    unit.split(|byte| *byte == b'\n').next() == Some(MARKER.as_bytes())
}

/// Fails when either door path holds a file that is not ReviewQ's. A missing file is fine.
pub(super) fn check(paths: &Paths) -> Result<(), Failure> {
    for path in [&paths.start_socket, &paths.start_door] {
        if files::read_unit(path)?.is_some_and(|unit| !owned(&unit)) {
            return Err(paths::conflict(path));
        }
    }
    Ok(())
}

/// Whether either door file differs from what the current settings want.
fn differs(paths: &Paths) -> Result<bool, Failure> {
    for (path, wanted) in wanted(paths)? {
        if files::read_unit(path)?.as_ref() != Some(&wanted) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Writes each door file that differs from what the current settings want, after checking
/// that both are ReviewQ's. Returns whether it wrote. The caller reloads the user manager.
pub(super) fn write(paths: &Paths) -> Result<bool, Failure> {
    check(paths)?;
    let mut written = false;
    for (path, wanted) in wanted(paths)? {
        let existing = files::read_unit(path)?;
        if existing.as_ref() == Some(&wanted) {
            continue;
        }
        paths::ensure_dir(paths::parent(path)?, false)?;
        paths::atomic_file(path, &wanted, existing.is_some())?;
        written = true;
    }
    Ok(written)
}

/// Stops the start socket, then any door still answering a connection it accepted, so
/// nothing starts the service while an install, rollback or uninstall works.
pub(super) async fn stop() -> Result<(), Failure> {
    stop_socket().await?;
    // Tolerated: with no door running there may be nothing to stop.
    let _ = service::control("stop", service::START_DOOR_INSTANCES).await;
    Ok(())
}

/// Stops the start socket when it is listening. A unit the user manager does not know is
/// not listening.
async fn stop_socket() -> Result<(), Failure> {
    if service::unit_state(service::START_SOCKET).await?.1 {
        service::control("stop", service::START_SOCKET).await?;
    }
    Ok(())
}

/// Enables the start socket for every login and starts it listening. A failure, such as
/// another program on the start port, does not hold up the service: it is reported, and the
/// repair at the next start tries again.
pub(super) async fn open() {
    let opened = async {
        service::control("enable", service::START_SOCKET).await?;
        service::control("start", service::START_SOCKET).await
    };
    if let Err(error) = opened.await {
        report_shut(&error);
    }
}

/// Says that the door is shut until the next start, whose repair tries again.
pub(super) fn report_shut(error: &Failure) {
    eprintln!("ReviewQ's Start button will not work until its next start: {error}");
}

/// Stops and disables the start door before its files are removed.
pub(super) async fn retire() -> Result<(), Failure> {
    stop().await?;
    if service::unit_state(service::START_SOCKET).await?.0 {
        service::control("disable", service::START_SOCKET).await?;
    }
    Ok(())
}

/// How long a release's program may take to answer whether it has the door.
const PROBE_WAIT: Duration = Duration::from_secs(5);

/// Whether the release whose program is `binary` has the start door, asked of that program:
/// `start-door --help` exits 0 only where the command exists, and help never runs it. A probe
/// and not a record, because a release from before the door reads the install record during
/// the rollback that needs the answer and refuses any field it does not know. Anything that
/// stops the program from answering counts as no door, which leaves nothing behind.
pub(super) async fn supported(binary: &Path) -> bool {
    let mut command = tokio::process::Command::new(binary);
    command
        .args(["start-door", "--help"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    matches!(
        tokio::time::timeout(PROBE_WAIT, command.status()).await,
        Ok(Ok(status)) if status.success()
    )
}

/// Removes ReviewQ's two door files. The caller reloads the user manager.
pub(super) fn remove(paths: &Paths) -> Result<(), Failure> {
    check(paths)?;
    for path in [&paths.start_socket, &paths.start_door] {
        if paths::exists(path)? {
            fs::remove_file(path)?;
            paths::sync(paths::parent(path)?)?;
        }
    }
    Ok(())
}

/// Puts the start door back as the current `runtime.json` wants it: rewrites the files when
/// either differs, enables the socket, clears a failed state such as a hit start limit, and
/// starts it listening. Works while `reviewq.service` runs. Fails without changing anything
/// when either file is not ReviewQ's. A failure after it stopped the socket still tries to
/// start it again, so a failed repair does not leave the door shut.
pub(crate) async fn repair(paths: &Paths) -> Result<(), Failure> {
    check(paths)?;
    let rewrite = differs(paths)?;
    if rewrite {
        stop_socket().await?;
    }
    let repaired = async {
        if rewrite {
            write(paths)?;
            service::reload().await?;
        }
        let (enabled, active) = service::unit_state(service::START_SOCKET).await?;
        if !enabled {
            service::control("enable", service::START_SOCKET).await?;
        }
        if service::failed(service::START_SOCKET).await? {
            service::control("reset-failed", service::START_SOCKET).await?;
        }
        if !active {
            service::control("start", service::START_SOCKET).await?;
        }
        Ok(())
    }
    .await;
    if repaired.is_err() && rewrite {
        let _ = service::control("start", service::START_SOCKET).await;
    }
    repaired
}

/// How often the service's repair asks for the installation lock, and for how long.
const LOCK_RETRY: Duration = Duration::from_secs(2);
const LOCK_WAIT: Duration = Duration::from_secs(120);

/// The installed service's repair after its dashboard is ready. Looks first, without the
/// installation lock, so a sound door never holds the lock; then repairs under it. An install
/// or rollback starts this service while it still holds the lock, so the repair asks again
/// for up to two minutes; after that the next start repairs.
pub(crate) async fn repair_after_start(paths: Paths) {
    let repaired = async {
        let (enabled, active) = service::unit_state(service::START_SOCKET).await?;
        if enabled && active && !differs(&paths)? {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + LOCK_WAIT;
        loop {
            if let Some(_lock) = paths.try_lock()? {
                return repair(&paths).await;
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(());
            }
            tokio::time::sleep(LOCK_RETRY).await;
        }
    };
    if let Err(error) = repaired.await {
        eprintln!("ReviewQ could not repair its start door: {error}");
    }
}
