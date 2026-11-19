//! Companion proof against the selected local Claude Code profile.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::hosts::{ClaudeCompanion, ClaudeContext, HostProof};
use reviewq_domain::session::{AbsolutePath, HostProvider, NativeBinding, NativeIdentity};
use tokio::sync::Semaphore;

use crate::history::{Cancel, verified_claude_source};

pub struct Claude {
    profile: AbsolutePath,
    reads: Arc<Semaphore>,
}

impl Claude {
    /// Selects one owned Claude config root without starting a native process.
    pub fn new(profile: PathBuf) -> Result<Self, AppError> {
        let profile = profile.canonicalize().map_err(|_| AppError::Unavailable)?;
        let metadata = profile.metadata().map_err(|_| AppError::Unavailable)?;
        if !metadata.is_dir()
            || metadata.mode() & 0o022 != 0
            || metadata.uid() != rustix::process::getuid().as_raw()
        {
            return Err(AppError::Forbidden);
        }
        Ok(Self {
            profile: AbsolutePath::from_canonical(profile).map_err(|_| AppError::InvalidRequest)?,
            reads: Arc::new(Semaphore::new(4)),
        })
    }
}

impl ClaudeCompanion for Claude {
    fn prove(&self, context: ClaudeContext) -> AppFuture<'_, HostProof> {
        Box::pin(async move {
            if context.profile != self.profile {
                return Err(AppError::Forbidden);
            }
            let session = context.session_id.as_str();
            if !uuid::Uuid::parse_str(session)
                .is_ok_and(|id| id.hyphenated().to_string() == session)
            {
                return Err(AppError::InvalidRequest);
            }
            let permit = self
                .reads
                .clone()
                .try_acquire_owned()
                .map_err(|_| AppError::Busy)?;
            let cancel = Cancel(Arc::new(AtomicBool::new(false)));
            let stopped = cancel.0.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let identity =
                    NativeIdentity::new(HostProvider::Claude, context.profile, context.session_id);
                let binding = NativeBinding::new(identity, context.cwd);
                let source =
                    verified_claude_source(&binding, context.transcript.as_ref(), &stopped)?
                        .ok_or(AppError::NotFound)?;
                Ok(HostProof {
                    binding,
                    source: Some(source),
                    activity: None,
                })
            })
            .await
            .map_err(|_| AppError::Unavailable)?
        })
    }
}
