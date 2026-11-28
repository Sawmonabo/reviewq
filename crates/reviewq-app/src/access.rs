//! Trusted application actors and safe errors at the adapter boundary.

use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::pin::Pin;

use reviewq_domain::identity::{GrantId, Revision};
use reviewq_domain::session::NativeIdentity;

/// A future returned by an async adapter port.
pub type AppFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AppError>> + Send + 'a>>;

/// Linux filesystem types admitted for large disposable disk-backed reader files.
pub fn admits_disk_scratch(filesystem: u64) -> bool {
    // ext, XFS, Btrfs, F2FS and ZFS.
    matches!(
        filesystem,
        0xef53 | 0x5846_5342 | 0x9123_683e | 0xf2f5_2010 | 0x2fc1_2fc1
    )
}

/// Stable authority for idempotency and scope checks. This contains no credentials.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ActorScope {
    /// The single installation operator across browser and CLI requests.
    LocalOperator,
    /// One checkout grant, whose current epoch and configured scope must be rechecked.
    CheckoutGrant(GrantId),
    /// One exact provider, profile and native session proved by the host adapter.
    NativeSession(NativeIdentity),
}

/// A principal established by an entry adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Actor {
    scope: ActorScope,
    grant_epoch: Option<Revision>,
}

impl Actor {
    pub fn local_operator() -> Self {
        Self {
            scope: ActorScope::LocalOperator,
            grant_epoch: None,
        }
    }

    pub fn checkout_grant(grant_id: GrantId, epoch: Revision) -> Self {
        Self {
            scope: ActorScope::CheckoutGrant(grant_id),
            grant_epoch: Some(epoch),
        }
    }

    pub fn native_session(identity: NativeIdentity) -> Self {
        Self {
            scope: ActorScope::NativeSession(identity),
            grant_epoch: None,
        }
    }

    pub fn scope(&self) -> &ActorScope {
        &self.scope
    }

    pub fn grant_epoch(&self) -> Option<Revision> {
        self.grant_epoch
    }
}

/// A tool input field that breaks a rule, with the change the agent can make in its
/// open turn. The path is a JSON pointer into the tool input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldError {
    pub path: String,
    pub message: String,
}

impl FieldError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Safe application outcomes translated into protocol errors by transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppError {
    InvalidRequest,
    /// Tool input fields the agent corrects in its open turn. Nothing was written. Return
    /// every error found; the transport caps the list where it writes the reply.
    InvalidFields(Vec<FieldError>),
    TicketUnverified,
    Unauthorized,
    Forbidden,
    NotFound,
    /// A session's transcript is not found because the host's session folder holds more
    /// entries than ReviewQ searches. Repeating the call finds no more, so it is not retryable.
    SessionFolderTooLarge,
    Conflict,
    StaleRevision,
    IdempotencyConflict,
    CursorExpired,
    PreconditionFailed,
    /// A call the host's transcript records under another ReviewQ tool's name: the session
    /// still holds the tools of a plugin loaded before the current ones.
    OutdatedTool,
    PayloadTooLarge,
    RangeNotSatisfiable,
    Busy,
    Unavailable,
    Internal,
}

impl Display for AppError {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidRequest => "invalid request",
            Self::InvalidFields(_) => "Invalid tool input.",
            Self::TicketUnverified => {
                "Could not verify a Jira key in this merge request. Check the Jira connection and issue access."
            }
            Self::Unauthorized => "authentication required",
            Self::Forbidden => "forbidden",
            Self::NotFound => "resource not found",
            Self::SessionFolderTooLarge => {
                "ReviewQ could not find this session's transcript: the host's session folder holds more entries than ReviewQ searches. Move older sessions out of that folder, then try again."
            }
            Self::Conflict => "resource conflict",
            Self::StaleRevision => "source revision changed",
            Self::IdempotencyConflict => "submission ID already used for different input",
            Self::CursorExpired => "cursor expired",
            Self::PreconditionFailed => "resource precondition failed",
            Self::OutdatedTool => "the session holds an earlier version of ReviewQ's tools",
            Self::PayloadTooLarge => "payload exceeds its limit",
            Self::RangeNotSatisfiable => "requested range is unavailable",
            Self::Busy => "service is busy",
            Self::Unavailable => "resource unavailable",
            Self::Internal => "internal service error",
        };
        output.write_str(message)
    }
}

impl std::error::Error for AppError {}
