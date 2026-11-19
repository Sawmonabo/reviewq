//! Companion proof against the selected local Claude Code profile.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{
    ClaudeAnchor, ClaudeChild, ClaudeCompanion, ClaudeContext, HostProof, MessageBody,
    ProvenClaudeChild,
};
use reviewq_domain::identity::NativeInputId;
use reviewq_domain::session::{AbsolutePath, HostProvider, NativeBinding, NativeIdentity};
use tokio::sync::Semaphore;

use crate::history::{
    Cancel, claude_anchor, claude_confirm_prompt, verified_claude_child, verified_claude_source,
};

mod cli;
pub(crate) mod jobs;
mod workflow;
pub use cli::ClaudeCli;
pub use workflow::ClaudeWorkflow;

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

impl Claude {
    fn read_native<T: Send + 'static>(
        &self,
        session: HistorySession,
        operation: impl FnOnce(HistorySession, &AtomicBool) -> Result<T, AppError> + Send + 'static,
    ) -> AppFuture<'_, T> {
        Box::pin(async move {
            if session.binding.identity().profile() != &self.profile || session.source.is_none() {
                return Err(AppError::Forbidden);
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
                operation(session, &stopped)
            })
            .await
            .map_err(|_| AppError::Unavailable)?
        })
    }
}

impl ClaudeCompanion for Claude {
    fn prove_child(
        &self,
        root: HistorySession,
        child: ClaudeChild,
    ) -> AppFuture<'_, ProvenClaudeChild> {
        self.read_native(root, move |root, cancelled| {
            let cwd = child
                .cwd
                .as_path()
                .canonicalize()
                .map_err(|_| AppError::NotFound)?;
            let cwd = AbsolutePath::from_canonical(cwd).map_err(|_| AppError::Forbidden)?;
            let binding = NativeBinding::new(
                NativeIdentity::new(
                    HostProvider::Claude,
                    root.binding.identity().profile().clone(),
                    child.id,
                ),
                cwd,
            );
            let source = verified_claude_child(&root, &binding, cancelled)?;
            Ok(ProvenClaudeChild {
                generation: child.generation,
                sequence: child.sequence,
                binding,
                parent: child.parent,
                source,
                working: child.working,
            })
        })
    }

    fn anchor(&self, session: HistorySession) -> AppFuture<'_, ClaudeAnchor> {
        self.read_native(session, |session, cancelled| {
            claude_anchor(&session, cancelled)
        })
    }

    fn confirm_prompt(
        &self,
        session: HistorySession,
        anchor: ClaudeAnchor,
        entry: NativeInputId,
        text: MessageBody,
    ) -> AppFuture<'_, ()> {
        self.read_native(session, move |session, cancelled| {
            claude_confirm_prompt(&session, &anchor, &entry, &text, cancelled)
        })
    }

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
