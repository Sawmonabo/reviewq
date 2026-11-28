use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use reviewq_app::access::AppError;
use reviewq_domain::session::AbsolutePath;

mod attention;
mod catalog;
mod children;
mod connection;
mod delivery;
mod history;
mod history_navigation;
mod history_reads;
mod history_search;
mod hook_reload;
mod hook_trust;
mod launch;
mod live;
mod questions;
mod recovery;
mod session;
mod turn_lookup;
mod workflow;

pub use hook_trust::{HookRevocation, approve_hooks, revoke_hooks};

pub struct Codex {
    executable: PathBuf,
    profile: AbsolutePath,
    hook_command: Option<String>,
    last_hook_check: hook_trust::LastCheck,
    connection: connection::Connection,
    question_proofs: tokio::sync::Semaphore,
    capacity: tokio::sync::Mutex<Option<(std::time::Instant, recovery::Usage)>>,
    turn_settings: Option<(String, String)>,
    turn_lookups: turn_lookup::TurnLookups,
    document: std::sync::Arc<tokio::sync::Mutex<Option<history_reads::PreparedDocument>>>,
    restore_grace: std::time::Duration,
}

impl Codex {
    /// Selects one provider executable and profile without starting a host or a turn.
    pub fn new(
        executable: PathBuf,
        profile: PathBuf,
        turn_settings: Option<(String, String)>,
    ) -> Result<Self, AppError> {
        let executable = executable
            .canonicalize()
            .map_err(|_| AppError::Unavailable)?;
        if !executable.is_file() {
            return Err(AppError::InvalidRequest);
        }
        let profile = profile.canonicalize().map_err(|_| AppError::Unavailable)?;
        let metadata = profile.metadata().map_err(|_| AppError::Unavailable)?;
        if !metadata.is_dir() || metadata.mode() & 0o022 != 0 {
            return Err(AppError::Forbidden);
        }
        if turn_settings.as_ref().is_some_and(|(model, effort)| {
            model.is_empty()
                || model.len() > 256
                || model.chars().any(char::is_control)
                || !matches!(
                    effort.as_str(),
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
                )
        }) {
            return Err(AppError::InvalidRequest);
        }
        Ok(Self {
            connection: connection::Connection::new(
                executable.clone(),
                profile.clone(),
                metadata.uid(),
            ),
            executable,
            hook_command: None,
            last_hook_check: Default::default(),
            question_proofs: tokio::sync::Semaphore::new(4),
            capacity: tokio::sync::Mutex::new(None),
            profile: AbsolutePath::from_canonical(profile).map_err(|_| AppError::InvalidRequest)?,
            turn_settings,
            turn_lookups: turn_lookup::TurnLookups::default(),
            document: Default::default(),
            restore_grace: attention::RESTORE_GRACE,
        })
    }

    /// Names the command of ReviewQ's own hooks in this profile, which `approve_hooks` may
    /// trust. Only the installed service knows it.
    pub fn with_hook_command(mut self, command: Option<String>) -> Self {
        self.hook_command = command;
        self
    }
}
