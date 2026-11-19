use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use reviewq_app::access::AppError;
use reviewq_domain::session::AbsolutePath;

mod attention;
mod catalog;
mod connection;
mod delivery;
mod history;
mod history_navigation;
mod history_reads;
mod launch;
mod live;
mod questions;
mod recovery;
mod session;
mod workflow;

pub struct Codex {
    profile: AbsolutePath,
    connection: connection::Connection,
    question_proofs: tokio::sync::Semaphore,
    capacity: tokio::sync::Mutex<Option<(std::time::Instant, recovery::Usage)>>,
    turn_settings: Option<(String, String)>,
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
            connection: connection::Connection::new(executable, profile.clone(), metadata.uid()),
            question_proofs: tokio::sync::Semaphore::new(4),
            capacity: tokio::sync::Mutex::new(None),
            profile: AbsolutePath::from_canonical(profile).map_err(|_| AppError::InvalidRequest)?,
            turn_settings,
        })
    }
}
