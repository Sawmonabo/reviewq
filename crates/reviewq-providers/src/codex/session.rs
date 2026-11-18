use std::path::PathBuf;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::hosts::NativeHost;
use reviewq_domain::identity::NativeSessionId;
use reviewq_domain::session::{AbsolutePath, HostProvider, NativeBinding, NativeIdentity};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::Codex;

#[derive(Deserialize)]
struct ThreadRead {
    thread: Thread,
}

#[derive(Deserialize)]
struct Thread {
    id: String,
    cwd: PathBuf,
}

impl NativeHost for Codex {
    fn prove(&self, native_id: NativeSessionId) -> AppFuture<'_, NativeBinding> {
        Box::pin(async move {
            validate_native_id(&native_id)?;
            let found: ThreadRead = self
                .connection
                .read(
                    "thread/read",
                    json!({"threadId":native_id.as_str(),"includeTurns":false}),
                )
                .await?;
            if found.thread.id != native_id.as_str() {
                return Err(AppError::Conflict);
            }
            let cwd = tokio::fs::canonicalize(found.thread.cwd)
                .await
                .map_err(|_| AppError::Unavailable)?;
            if !cwd.is_dir() {
                return Err(AppError::Unavailable);
            }
            let cwd = AbsolutePath::from_canonical(cwd).map_err(|_| AppError::Unavailable)?;
            Ok(NativeBinding::new(
                NativeIdentity::new(HostProvider::Codex, self.profile.clone(), native_id),
                cwd,
            ))
        })
    }
}

pub(super) fn validate_native_id(id: &NativeSessionId) -> Result<(), AppError> {
    let value = Uuid::parse_str(id.as_str()).map_err(|_| AppError::InvalidRequest)?;
    if value.is_nil() || value.hyphenated().to_string() != id.as_str() {
        return Err(AppError::InvalidRequest);
    }
    Ok(())
}
