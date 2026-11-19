use reviewq_domain::identity::{NativeTurnId, SessionKey};

use crate::access::{AppError, AppFuture};
use crate::history::HistorySession;
use crate::hosts::{HostActivity, HostProof, NativeSessionMetadata};
use crate::workflow::NativeWorkflowProof;

use super::Kind;

pub struct TrackedNative {
    pub session: HistorySession,
    pub known_failure: Option<NativeTurnId>,
    pub child_cursor: Option<NativeChildCursor>,
}

pub struct NativeChildCursor {
    pub cursor: String,
    pub archived: bool,
}

pub struct DiscoveredChildren {
    pub children: Vec<HostProof>,
    pub next: Option<NativeChildCursor>,
}

pub struct NativeAttention {
    pub activity: Option<HostActivity>,
    pub metadata: NativeSessionMetadata,
    pub children: Option<DiscoveredChildren>,
    pub terminal_failure: Option<NativeTurnId>,
    pub recoverable: Option<crate::hosts::recovery::NativeFailure>,
    pub events: Vec<NativeOccurrence>,
}

pub struct NativeOccurrence {
    pub kind: Kind,
    pub native_ref: String,
    pub at_ms: Option<i64>,
    pub desktop_episode: Option<String>,
}

pub trait NativeAttentionStore: Send + Sync {
    fn tracked_native(&self, after: Option<SessionKey>) -> AppFuture<'_, Vec<TrackedNative>>;
    fn record_native_attention(
        &self,
        target: HistorySession,
        observed: NativeAttention,
    ) -> AppFuture<'_, ()>;
}

pub async fn poll_native_attention(
    store: &dyn NativeAttentionStore,
    native: &dyn NativeWorkflowProof,
    after: Option<SessionKey>,
) -> Result<Option<SessionKey>, AppError> {
    let rows = store.tracked_native(after).await?;
    let next = rows
        .last()
        .map(|row| row.session.target.session_key().clone());
    for row in rows {
        let session = row.session.clone();
        if let Ok(Some(observed)) = native.observe_attention(row).await
            && let Err(error) = store.record_native_attention(session, observed).await
            && !matches!(error, AppError::NotFound | AppError::StaleRevision)
        {
            return Err(error);
        }
    }
    Ok(next)
}
