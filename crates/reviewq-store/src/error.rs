//! Internal storage failures and safe adapter outcomes.

use std::path::{Path, PathBuf};

use reviewq_app::access::AppError;

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct StoreError(#[from] Problem);

#[derive(Debug, thiserror::Error)]
enum Problem {
    #[error("store path {path:?}: {reason}")]
    Path { path: PathBuf, reason: &'static str },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    App(#[from] AppError),
}

impl StoreError {
    pub(crate) fn path(path: &Path, reason: &'static str) -> Self {
        Problem::Path {
            path: path.into(),
            reason,
        }
        .into()
    }

    pub(crate) fn app(self) -> AppError {
        match self.0 {
            Problem::App(error) => error,
            Problem::Sqlite(error) => match error.sqlite_error_code() {
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                    AppError::Busy
                }
                _ => AppError::Internal,
            },
            Problem::Io(_) | Problem::Path { .. } => AppError::Internal,
        }
    }

    pub(crate) fn is_sqlite(&self) -> bool {
        matches!(self.0, Problem::Sqlite(_))
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Problem::from(error).into()
    }
}

impl From<rustix::io::Errno> for StoreError {
    fn from(error: rustix::io::Errno) -> Self {
        std::io::Error::from(error).into()
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Problem::from(error).into()
    }
}

impl From<AppError> for StoreError {
    fn from(error: AppError) -> Self {
        Problem::from(error).into()
    }
}
