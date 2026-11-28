//! `reviewq-start.socket`, the start door: systemd holds the start port while ReviewQ is
//! stopped, and a connection to it starts `reviewq.service`. The file has no install record
//! entry; it is ReviewQ's when rendering it for its own port gives the same bytes.

use std::fs;

use super::{
    Failure, files,
    paths::{self, Paths},
};
use crate::{runtime_settings, service};

const LISTEN: &str = "ListenStream=127.0.0.1:";

/// The unit for a start door on `port`. `FlushPending` drops connections still waiting when
/// the service exits, so one that arrives during a Quit cannot start ReviewQ again.
fn render(port: u16) -> Vec<u8> {
    format!(
        "[Unit]\nDescription=ReviewQ start door\n\n[Socket]\n{LISTEN}{port}\nService=reviewq.service\nFlushPending=yes\n\n[Install]\nWantedBy=sockets.target\n"
    )
    .into_bytes()
}

/// Whether `unit` is ReviewQ's: the render for the port it names.
fn owned(unit: &[u8]) -> bool {
    std::str::from_utf8(unit)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix(LISTEN))
                .and_then(|port| port.parse::<u16>().ok())
        })
        .is_some_and(|port| render(port) == unit)
}

/// The unit for the start port of the browser port in `runtime.json`.
fn wanted(paths: &Paths) -> Result<Vec<u8>, Failure> {
    let port = runtime_settings::Settings::load(&paths.profile, false)?.port;
    let start = runtime_settings::start_port(port)
        .ok_or("The browser port leaves no room for the start port.")?;
    Ok(render(start))
}

/// Fails when a start socket file that is not ReviewQ's is in ReviewQ's place.
pub(super) fn check(paths: &Paths) -> Result<(), Failure> {
    match files::read_unit(&paths.start_socket)? {
        Some(unit) if !owned(&unit) => Err(paths::conflict(&paths.start_socket)),
        _ => Ok(()),
    }
}

/// Writes the unit for the current port when the file is missing or names another port.
/// Returns whether it wrote. The caller reloads the user manager after a write.
pub(super) fn write(paths: &Paths) -> Result<bool, Failure> {
    let wanted = wanted(paths)?;
    let existing = files::read_unit(&paths.start_socket)?;
    match &existing {
        Some(unit) if *unit == wanted => return Ok(false),
        Some(unit) if !owned(unit) => return Err(paths::conflict(&paths.start_socket)),
        _ => {}
    }
    paths::ensure_dir(paths::parent(&paths.start_socket)?, false)?;
    paths::atomic_file(&paths.start_socket, &wanted, existing.is_some())?;
    Ok(true)
}

/// Stops the start socket when it is listening, so no connection starts the service.
pub(super) async fn stop(paths: &Paths) -> Result<(), Failure> {
    if paths::exists(&paths.start_socket)? && service::unit_state(service::START_SOCKET).await?.1 {
        service::control("stop", service::START_SOCKET).await?;
    }
    Ok(())
}

/// Enables the start socket for every login and starts it listening, before the service
/// starts. A failure, such as another program on the start port, does not hold up the
/// service: it is reported, and the repair at the next start tries again.
pub(super) async fn open() {
    let opened = async {
        service::control("enable", service::START_SOCKET).await?;
        service::control("start", service::START_SOCKET).await
    };
    if let Err(error) = opened.await {
        eprintln!("ReviewQ's Start button will not work until its next start: {error}");
    }
}

/// Stops and disables the start socket before an uninstall removes anything else.
pub(super) async fn retire(paths: &Paths) -> Result<(), Failure> {
    if !paths::exists(&paths.start_socket)? {
        return Ok(());
    }
    stop(paths).await?;
    if service::unit_state(service::START_SOCKET).await?.0 {
        service::control("disable", service::START_SOCKET).await?;
    }
    Ok(())
}

/// Removes ReviewQ's start socket file. The caller reloads the user manager.
pub(super) fn remove(paths: &Paths) -> Result<(), Failure> {
    check(paths)?;
    if paths::exists(&paths.start_socket)? {
        fs::remove_file(&paths.start_socket)?;
        paths::sync(paths::parent(&paths.start_socket)?)?;
    }
    Ok(())
}

/// Puts the start door back as the current `runtime.json` wants it: rewrites the unit for a
/// changed port, enables it, clears a failed state such as a hit start limit, and starts it
/// listening. A running service keeps the descriptor it was given; the next start gets the
/// repaired one. Fails without changing anything when the file is not ReviewQ's.
pub(crate) async fn repair(paths: &Paths) -> Result<(), Failure> {
    check(paths)?;
    if wanted(paths)? != files::read_unit(&paths.start_socket)?.unwrap_or_default() {
        stop(paths).await?;
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
