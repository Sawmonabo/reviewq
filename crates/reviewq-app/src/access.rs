//! Trusted application actors and safe errors at the adapter boundary.

use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

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
    /// A problem with GitLab or Jira that the person can act on, in its own sentence.
    Source(SourceProblem),
    Internal,
}

/// The outside service a [`SourceProblem`] names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Service {
    GitLab,
    Jira,
}

impl Service {
    fn name(self) -> &'static str {
        match self {
            Self::GitLab => "GitLab",
            Self::Jira => "Jira",
        }
    }
}

/// Why GitLab or Jira gave no data, apart from a token refused by the API itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceProblem {
    /// Settings cannot save the address: it is not `https://` and a host alone.
    Address(Service),
    /// The saved address answered with a redirect, which ReviewQ never follows, or with a
    /// web page where the API answers JSON.
    WrongAddress(Service),
    /// Atlassian's API gateway, which a scoped Jira token reads through, answered with a
    /// redirect or a web page.
    GatewayPage,
    /// A bot check in front of the service answered instead of its API.
    BotCheck(Service),
    /// A 403 web page refused the request: the service, or something in front of it.
    RefusedPage(Service),
    /// The service's API refused access during a queue refresh: a 401, or a 403 in its JSON.
    /// The refresh names the service, as a plain `Forbidden` cannot.
    Refused(Service),
    /// The service asked ReviewQ to wait, for its rate limit or with `Retry-After`, longer
    /// than the call can: for this long, when the asking call knows it.
    Wait(Service, Option<Duration>),
}

impl SourceProblem {
    /// The wait left before the service takes requests again, when this is a known wait.
    pub fn retry_after(self) -> Option<Duration> {
        match self {
            Self::Wait(_, wait) => wait,
            _ => None,
        }
    }
}

impl Display for SourceProblem {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Address(Service::GitLab) => output.write_str(
                "Save the GitLab address as https:// and the host, with nothing after the host, such as https://gitlab.com.",
            ),
            Self::Address(Service::Jira) => output.write_str(
                "Save the Jira site as https:// and the host, with nothing after the host, such as https://team.atlassian.net.",
            ),
            Self::WrongAddress(Service::GitLab) => output.write_str(
                "A request to the saved GitLab address came back with a redirect or a web page instead of GitLab's data. Open that address in a browser and save the address GitLab's own pages end up on, with nothing after the host. If that is already the saved address, something in front of GitLab answered: check the VPN, proxy or allow-list this machine uses.",
            ),
            Self::WrongAddress(Service::Jira) => output.write_str(
                "A request to the saved Jira site came back with a redirect or a web page instead of Jira Cloud's data. Open that address in a browser and save the address your Jira Cloud site ends up on, such as https://team.atlassian.net; ReviewQ reads Jira Cloud only. If that is already the saved address, something in front of Jira answered: check the VPN, proxy or allow-list this machine uses.",
            ),
            Self::GatewayPage => output.write_str(
                "A request to Atlassian's API gateway, which a scoped Jira token reads through, came back with a redirect or a web page instead of Jira's data. Try again later; if it keeps happening, check the VPN, proxy or allow-list this machine uses.",
            ),
            Self::Refused(service) => write!(
                output,
                "{0} refused access with the saved token during the last refresh. Check the {0} token and its access in Settings.",
                service.name()
            ),
            Self::BotCheck(service) => write!(
                output,
                "A bot check in front of {} stopped this request. Try again later; if it keeps happening, check the VPN or proxy this machine uses.",
                service.name()
            ),
            Self::RefusedPage(service) => write!(
                output,
                "{0}, or something in front of {0}, refused this request with a web page. Check the {0} token in Settings, then the VPN, proxy or allow-list this machine uses.",
                service.name()
            ),
            Self::Wait(service, Some(wait)) => write!(
                output,
                "{} asked ReviewQ to wait before its next request. Try again {}; ReviewQ's background refresh waits on its own.",
                service.name(),
                in_words(wait)
            ),
            Self::Wait(service, None) => write!(
                output,
                "{} asked ReviewQ to wait before its next request. ReviewQ's background refresh tries again when the wait is over.",
                service.name()
            ),
        }
    }
}

/// A wait in plain words: minutes rounded up under two hours, so the person does not try too
/// early, and the nearest hour beyond: "in about a minute", "in about 12 minutes", "in about
/// 3 hours".
fn in_words(wait: Duration) -> String {
    let minutes = wait.as_secs().div_ceil(60).max(1);
    match minutes {
        1 => "in about a minute".into(),
        60 => "in about an hour".into(),
        2..120 => format!("in about {minutes} minutes"),
        _ => format!("in about {} hours", (minutes + 30) / 60),
    }
}

impl AppError {
    /// GitLab or Jira refused access: a 401, a 403 from its API, or a 403 web page that may
    /// be the service refusing the token.
    pub fn refuses_access(&self) -> bool {
        matches!(
            self,
            Self::Unauthorized
                | Self::Forbidden
                | Self::Source(SourceProblem::RefusedPage(_) | SourceProblem::Refused(_))
        )
    }

    /// This error, with a refusal from `service`'s API named as that service's.
    pub fn refused_by(self, service: Service) -> Self {
        match self {
            Self::Unauthorized | Self::Forbidden => Self::Source(SourceProblem::Refused(service)),
            error => error,
        }
    }

    /// A source read that gave no data this time, for a caller that shows what it has
    /// without that part instead of failing: missing, refused, unreachable, or answered by a
    /// page that is not the API. A wait is not one, so the caller can honour it.
    pub fn source_unreadable(&self) -> bool {
        matches!(
            self,
            Self::Unavailable
                | Self::NotFound
                | Self::Unauthorized
                | Self::Forbidden
                | Self::Source(
                    SourceProblem::WrongAddress(_)
                        | SourceProblem::GatewayPage
                        | SourceProblem::BotCheck(_)
                        | SourceProblem::RefusedPage(_)
                        | SourceProblem::Refused(_)
                )
        )
    }
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
            Self::Source(problem) => return problem.fmt(output),
            Self::Internal => "internal service error",
        };
        output.write_str(message)
    }
}

impl std::error::Error for AppError {}
