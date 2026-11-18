use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use reviewq_app::access::AppError;
use reviewq_domain::session::AbsolutePath;

mod connection;
mod session;

pub struct Codex {
    profile: AbsolutePath,
    connection: connection::Connection,
}

impl Codex {
    /// Selects one provider executable and profile without starting a host or a turn.
    pub fn new(executable: PathBuf, profile: PathBuf) -> Result<Self, AppError> {
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
        Ok(Self {
            connection: connection::Connection::new(executable, profile.clone(), metadata.uid()),
            profile: AbsolutePath::from_canonical(profile).map_err(|_| AppError::InvalidRequest)?,
        })
    }
}
