use reviewq_domain::identity::{NativeTurnId, SessionKey};

use crate::access::{AppError, AppFuture};
use crate::history::HistorySession;
use crate::hosts::{HostActivity, HostProof, NativeSessionMetadata, ObservedAsyncQuestion};
use crate::workflow::NativeWorkflowProof;

use super::Kind;

pub struct TrackedNative {
    pub session: HistorySession,
    pub known_failure: Option<NativeTurnId>,
    pub child_cursor: Option<NativeChildCursor>,
    pub started_at_ms: i64,
    pub questions: Vec<ObservedAsyncQuestion>,
    /// The session is its run's lead, and the run has an open pass that is not waiting, or
    /// unpublished records on a published version and no pass: whether the lead is idle can
    /// change what settling does. Only then does a provider read the saved turns of a thread
    /// its connection has not loaded to tell whether it is idle; otherwise such a thread is
    /// unknown.
    pub idle_decides: bool,
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
    pub async_questions: Vec<ObservedAsyncQuestion>,
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
