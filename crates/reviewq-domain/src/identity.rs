//! Validated identities shared within the service.

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

const MAX_ID_BYTES: usize = 512;
const MAX_SQLITE_INTEGER: u64 = i64::MAX as u64;

/// An identity or counter is outside its supported representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidIdentity;

impl Display for InvalidIdentity {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        output.write_str("invalid identity or counter")
    }
}

impl std::error::Error for InvalidIdentity {}

macro_rules! text_identity {
    ($(#[$doc:meta])* $name:ident, $validate:expr) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Box<str>);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = InvalidIdentity;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if !($validate)(value) {
                    return Err(InvalidIdentity);
                }
                Ok(Self(value.into()))
            }
        }

        impl TryFrom<&str> for $name {
            type Error = InvalidIdentity;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                value.parse()
            }
        }

        impl Display for $name {
            fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
                output.write_str(self.as_str())
            }
        }
    };
}

text_identity!(
    /// The path-safe ReviewQ identity for an exact registered merge request.
    ReviewId,
    path_segment
);
text_identity!(
    /// A registered run identity.
    RunId,
    opaque_id
);
text_identity!(
    /// The ReviewQ key for one exact native session within a run.
    SessionKey,
    opaque_id
);
text_identity!(
    /// The session identifier issued by a native host.
    NativeSessionId,
    opaque_id
);
text_identity!(
    /// The stable client retry key for one submission.
    SubmissionId,
    opaque_id
);
text_identity!(
    /// An independently authorized publication action.
    ActionId,
    opaque_id
);
text_identity!(
    /// The service receipt identifying one published report version.
    ReceiptId,
    opaque_id
);
text_identity!(
    /// Full SHA-256 of the exact UTF-8 report bytes.
    ReportHash,
    |value: &str| lowercase_hex(value, 64)
);
impl ReportHash {
    /// Formats a verified SHA-256 digest in the canonical hexadecimal representation.
    pub fn from_sha256(digest: [u8; 32]) -> Self {
        use std::fmt::Write as _;
        let mut text = String::with_capacity(64);
        for byte in digest {
            write!(&mut text, "{byte:02x}").expect("writing to a String");
        }
        Self(text.into_boxed_str())
    }
}

text_identity!(
    /// The stable checkout-grant ID. This contains no bearer credential.
    GrantId,
    opaque_id
);
text_identity!(
    /// Jira issue key used by queue and event associations.
    TicketKey,
    opaque_id
);
text_identity!(
    /// A public notification identity.
    NotificationId,
    opaque_id
);
text_identity!(
    /// The stable identity of a retained message in a registered session.
    MessageId,
    opaque_id
);
text_identity!(
    /// The service-issued UUID of one durable native outbox row.
    DeliveryId,
    canonical_uuid
);
text_identity!(
    /// A provider-native turn ID used in delivery proof.
    NativeTurnId,
    opaque_id
);
text_identity!(
    /// A provider-native user input ID used in delivery proof.
    NativeInputId,
    opaque_id
);

/// A full Git object name normalized to lowercase hexadecimal.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitSha(Box<str>);

impl GitSha {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for GitSha {
    type Err = InvalidIdentity;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(InvalidIdentity);
        }
        Ok(Self(value.to_ascii_lowercase().into_boxed_str()))
    }
}

impl TryFrom<&str> for GitSha {
    type Error = InvalidIdentity;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl Display for GitSha {
    fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
        output.write_str(self.as_str())
    }
}

macro_rules! bounded_integer {
    ($(#[$doc:meta])* $name:ident, $minimum:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
            pub const fn get(self) -> u64 {
                self.0
            }

            pub fn as_sqlite_integer(self) -> i64 {
                self.0 as i64
            }
        }

        impl TryFrom<u64> for $name {
            type Error = InvalidIdentity;

            fn try_from(value: u64) -> Result<Self, Self::Error> {
                if !($minimum..=MAX_SQLITE_INTEGER).contains(&value) {
                    return Err(InvalidIdentity);
                }
                Ok(Self(value))
            }
        }

        impl TryFrom<i64> for $name {
            type Error = InvalidIdentity;

            fn try_from(value: i64) -> Result<Self, Self::Error> {
                Self::try_from(u64::try_from(value).map_err(|_| InvalidIdentity)?)
            }
        }

        impl FromStr for $name {
            type Err = InvalidIdentity;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if value.is_empty()
                    || value.len() > 19
                    || (value.len() > 1 && value.starts_with('0'))
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(InvalidIdentity);
                }
                Self::try_from(value.parse::<u64>().map_err(|_| InvalidIdentity)?)
            }
        }

        impl Display for $name {
            fn fmt(&self, output: &mut Formatter<'_>) -> fmt::Result {
                Display::fmt(&self.0, output)
            }
        }
    };
}

bounded_integer!(
    /// A positive numeric project ID scoped to one verified GitLab origin.
    ProjectId,
    1
);
bounded_integer!(
    /// A positive merge-request IID scoped to one GitLab project.
    MergeRequestIid,
    1
);
bounded_integer!(
    /// A logical revision such as a settings or decision revision.
    Revision,
    0
);
bounded_integer!(
    /// The address in the durable global event journal.
    EventCursor,
    0
);
bounded_integer!(
    /// The monotonically advanced claim fence of one durable delivery.
    DeliveryFence,
    0
);

impl Revision {
    pub const ZERO: Self = Self(0);

    pub fn checked_add(self, increment: u64) -> Option<Self> {
        Self::try_from(self.0.checked_add(increment)?).ok()
    }
}

impl EventCursor {
    pub const ZERO: Self = Self(0);

    pub fn checked_add(self, increment: u64) -> Option<Self> {
        Self::try_from(self.0.checked_add(increment)?).ok()
    }
}

impl DeliveryFence {
    pub const ZERO: Self = Self(0);

    pub fn checked_add(self, increment: u64) -> Option<Self> {
        Self::try_from(self.0.checked_add(increment)?).ok()
    }
}

/// An HTTP(S) origin canonicalized and verified by the GitLab adapter.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GitLabOrigin(Box<str>);

impl GitLabOrigin {
    /// Accepts the serialization of a verified canonical URL origin.
    ///
    /// The adapter proves the URL grammar, hostname normalization, port and
    /// authority before calling this constructor. This checks the boundary
    /// representation without an independent URL parser.
    pub fn from_canonical(value: &str) -> Result<Self, InvalidIdentity> {
        let authority = value
            .strip_prefix("https://")
            .or_else(|| value.strip_prefix("http://"))
            .ok_or(InvalidIdentity)?;
        if authority.is_empty()
            || value.len() > MAX_ID_BYTES
            || !authority.bytes().all(|byte| byte.is_ascii_graphic())
            || authority.contains(['/', '?', '#', '@', '\\'])
        {
            return Err(InvalidIdentity);
        }
        Ok(Self(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The verified GitLab identity independent of titles, paths and ReviewQ IDs.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MrIdentity {
    origin: GitLabOrigin,
    project_id: ProjectId,
    iid: MergeRequestIid,
}

impl MrIdentity {
    pub fn new(origin: GitLabOrigin, project_id: ProjectId, iid: MergeRequestIid) -> Self {
        Self {
            origin,
            project_id,
            iid,
        }
    }

    pub fn origin(&self) -> &GitLabOrigin {
        &self.origin
    }

    pub fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub fn iid(&self) -> MergeRequestIid {
        self.iid
    }
}

fn opaque_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ID_BYTES && !value.chars().any(char::is_control)
}

fn path_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(is_lowercase_hex)
}

fn is_lowercase_hex(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                is_lowercase_hex(byte)
            }
        })
}
