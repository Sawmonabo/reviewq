//! The System page's controls for this process. Starting at login, Quit and Uninstall exist
//! only in the installed service, and each is refused while an install, update, uninstall,
//! start or stop holds the installation lock.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::system::{SystemControl, SystemFacts, SystemMode, SystemRefusal};
use tokio::sync::watch;

use crate::install::{self, data::Folders, paths::Paths};
use crate::service;

/// How long Quit waits for ReviewQ's own stop signal after the command that asked systemd
/// for the stop ended without success.
const STOP_SIGNAL: Duration = Duration::from_secs(2);

pub(super) struct System {
    mode: SystemMode,
    /// The installation's paths, in the installed service only.
    installed: Option<Paths>,
    folders: Folders,
    data_folder: String,
    /// The service's own shutdown, which SIGTERM also trips.
    shutdown: watch::Sender<bool>,
}

impl System {
    /// The controls for a server on `profile`. `installed` holds the installation's paths
    /// when this is the installed service; `shutdown` stops the server.
    pub(super) fn new(
        profile: &Path,
        validation: bool,
        installed: Option<Paths>,
        shutdown: watch::Sender<bool>,
    ) -> Self {
        let mode = if installed.is_some() {
            SystemMode::Installed
        } else if validation {
            SystemMode::Validation
        } else {
            SystemMode::Foreground
        };
        let folders = match installed.clone().or_else(|| {
            Paths::resolve()
                .ok()
                .filter(|paths| !validation && paths.profile == profile)
        }) {
            Some(paths) => Folders::installed(&paths),
            None => Folders::profile(profile),
        };
        Self {
            mode,
            installed,
            folders,
            data_folder: display(profile),
            shutdown,
        }
    }

    /// The installation's paths under the installation lock, or why the change is refused.
    fn hold(&self) -> Result<Result<(&Paths, File), SystemRefusal>, AppError> {
        let Some(paths) = &self.installed else {
            return Ok(Err(SystemRefusal::NotInstalled));
        };
        match paths.try_lock().map_err(failed)? {
            Some(lock) => Ok(Ok((paths, lock))),
            None => Ok(Err(SystemRefusal::InstallRunning)),
        }
    }
}

impl SystemControl for System {
    fn mode(&self) -> SystemMode {
        self.mode
    }

    fn facts(&self) -> AppFuture<'_, SystemFacts> {
        Box::pin(async move {
            let folders = self.folders.clone();
            let data_bytes = tokio::task::spawn_blocking(move || folders.size())
                .await
                .ok()
                .flatten();
            let start_at_login = if self.installed.is_some() {
                service::state().await.ok().map(|(enabled, _)| enabled)
            } else {
                None
            };
            Ok(SystemFacts {
                data_folder: self.data_folder.clone(),
                data_bytes,
                start_at_login,
            })
        })
    }

    fn set_start_at_login(&self, enabled: bool) -> AppFuture<'_, Result<bool, SystemRefusal>> {
        Box::pin(async move {
            let _lock = match self.hold()? {
                Ok((_, lock)) => lock,
                Err(refusal) => return Ok(Err(refusal)),
            };
            service::switch(if enabled { "enable" } else { "disable" })
                .await
                .map_err(failed)?;
            Ok(Ok(service::state().await.map_err(failed)?.0))
        })
    }

    fn quit(&self) -> AppFuture<'_, Result<(), SystemRefusal>> {
        Box::pin(async move {
            let _lock = match self.hold()? {
                Ok((_, lock)) => lock,
                Err(refusal) => return Ok(Err(refusal)),
            };
            // systemd makes the stop, so a start asked for while ReviewQ shuts down is queued
            // behind it. The stop signals every process in the service, the command that
            // asked included, so that command can die before it exits; ReviewQ's own stop
            // signal then shows the stop is under way.
            let mut stopping = self.shutdown.subscribe();
            let asked = tokio::select! {
                asked = service::stop_without_waiting() => asked,
                () = signalled(&mut stopping) => Ok(()),
            };
            if let Err(error) = asked
                && tokio::time::timeout(STOP_SIGNAL, signalled(&mut stopping))
                    .await
                    .is_err()
            {
                // systemd could not be asked. Exiting with status 0 is left stopped by
                // `Restart=on-failure`, so Quit still quits.
                eprintln!(
                    "ReviewQ could not ask systemd to stop it and is exiting itself: {error}"
                );
                self.shutdown.send_replace(true);
            }
            Ok(Ok(()))
        })
    }

    fn uninstall(&self, delete_data: bool) -> AppFuture<'_, Result<(), SystemRefusal>> {
        Box::pin(async move {
            let (paths, lock) = match self.hold()? {
                Ok(held) => held,
                Err(refusal) => return Ok(Err(refusal)),
            };
            let started = install::uninstall::start_job(paths, delete_data).await;
            drop(lock);
            match started {
                Ok(()) => Ok(Ok(())),
                // Only a refusal's sentence reaches the page; anything else is logged.
                Err(error) => match error.downcast::<install::Refusal>() {
                    Ok(refusal) => Ok(Err(SystemRefusal::Blocked(refusal.0))),
                    Err(error) => Err(failed(error)),
                },
            }
        })
    }
}

/// Resolves once the service's shutdown has been tripped.
async fn signalled(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
}

/// Logs a lifecycle failure and reports it as the internal error it is.
fn failed(error: install::Failure) -> AppError {
    eprintln!("ReviewQ could not complete a System page change: {error}");
    AppError::Internal
}

/// `path` for display, with the home folder shortened to `~`.
fn display(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
