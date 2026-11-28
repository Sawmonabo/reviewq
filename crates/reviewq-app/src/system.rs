//! Control of the running ReviewQ service from its System page: what is running, whether it
//! starts at login, quitting it and handing off its uninstall.

use std::fmt::{self, Display, Formatter};

use crate::access::AppFuture;

/// How this ReviewQ process was started.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemMode {
    /// The installed background service, started with `--managed`.
    Installed,
    /// A foreground `reviewq serve`.
    Foreground,
    /// A `reviewq serve --validation` test server.
    Validation,
}

/// What only the running process knows about itself, apart from its mode. The transport
/// adds the version, the dashboard address and the last refresh.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemFacts {
    /// The data folder for display, with the home folder shortened to `~`.
    pub data_folder: String,
    /// The total size of the data an uninstall with "delete my data" removes, or `None` when a
    /// size cannot be read.
    pub data_bytes: Option<u64>,
    /// Whether the service starts at login, read from systemd. `None` unless installed.
    pub start_at_login: Option<bool>,
}

/// An uninstall handed to a job outside the service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UninstallStarted {
    pub delete_data: bool,
    /// The data folder for display, as in `SystemFacts`.
    pub data_folder: String,
}

/// Why a System page change was refused before anything changed. Its text is the sentence the
/// page shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SystemRefusal {
    /// The process is not the installed service.
    NotInstalled,
    /// An install, update or uninstall holds the install lock.
    InstallRunning,
    /// An uninstall check failed; the text says what to do.
    Blocked(String),
}

/// The running service's own controls. Each change is refused with `SystemRefusal` unless the
/// process is the installed service and no install holds the install lock.
pub trait SystemControl: Send + Sync {
    /// How this process was started; it never changes while it runs.
    fn mode(&self) -> SystemMode;

    fn facts(&self) -> AppFuture<'_, SystemFacts>;

    /// Turns starting at login on or off without starting or stopping the service, and returns
    /// the state read back from systemd.
    fn set_start_at_login(&self, enabled: bool) -> AppFuture<'_, Result<bool, SystemRefusal>>;

    /// Stops the service: it finishes its open replies, this one included, then exits.
    fn quit(&self) -> AppFuture<'_, Result<(), SystemRefusal>>;

    /// Checks that an uninstall can run, then starts it as a job outside the service, which
    /// it stops.
    fn uninstall(
        &self,
        delete_data: bool,
    ) -> AppFuture<'_, Result<UninstallStarted, SystemRefusal>>;
}

impl Display for SystemRefusal {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::NotInstalled => "Available in the installed app.",
            Self::InstallRunning => {
                "An install or uninstall is running. Try again when it finishes."
            }
            Self::Blocked(message) => message,
        })
    }
}
