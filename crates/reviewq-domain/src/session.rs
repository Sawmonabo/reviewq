//! Private host provenance and exact registered message targets.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::identity::{InvalidIdentity, NativeSessionId, ReviewId, RunId, SessionKey};

/// The host that issues and owns a native session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HostProvider {
    Codex,
    Claude,
}

impl HostProvider {
    /// ReviewQ's key for this host's session `native`: the native ID, under `claude:` for a
    /// Claude session.
    pub fn session_key(self, native: &NativeSessionId) -> Result<SessionKey, InvalidIdentity> {
        match self {
            Self::Codex => native.as_str().parse(),
            Self::Claude => format!("claude:{native}").parse(),
        }
    }
}

/// A local path resolved to its canonical absolute form by a trusted adapter.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct AbsolutePath(PathBuf);

impl AbsolutePath {
    /// Checks path structure. The adapter proves filesystem identity.
    pub fn from_canonical(path: PathBuf) -> Result<Self, InvalidIdentity> {
        if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
            return Err(InvalidIdentity);
        }
        Ok(Self(path))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Debug for AbsolutePath {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("AbsolutePath(<private>)")
    }
}

/// The stable provider/profile/native identity established by a host adapter.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NativeIdentity {
    provider: HostProvider,
    profile: AbsolutePath,
    native_id: NativeSessionId,
}

impl NativeIdentity {
    pub fn new(provider: HostProvider, profile: AbsolutePath, native_id: NativeSessionId) -> Self {
        Self {
            provider,
            profile,
            native_id,
        }
    }

    pub fn provider(&self) -> HostProvider {
        self.provider
    }

    pub fn profile(&self) -> &AbsolutePath {
        &self.profile
    }

    pub fn native_id(&self) -> &NativeSessionId {
        &self.native_id
    }
}

/// A native identity with its verified working directory.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NativeBinding {
    identity: NativeIdentity,
    cwd: AbsolutePath,
}

impl NativeBinding {
    pub fn new(identity: NativeIdentity, cwd: AbsolutePath) -> Self {
        Self { identity, cwd }
    }

    pub fn identity(&self) -> &NativeIdentity {
        &self.identity
    }

    pub fn cwd(&self) -> &AbsolutePath {
        &self.cwd
    }
}

/// Exact run/session address verified under the selected review.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SessionTarget {
    review_id: ReviewId,
    run_id: RunId,
    session_key: SessionKey,
}

impl SessionTarget {
    pub fn new(review_id: ReviewId, run_id: RunId, session_key: SessionKey) -> Self {
        Self {
            review_id,
            run_id,
            session_key,
        }
    }

    pub fn review_id(&self) -> &ReviewId {
        &self.review_id
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn session_key(&self) -> &SessionKey {
        &self.session_key
    }
}

/// The immutable exact lead address and its verified provider binding.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LeadTarget {
    target: SessionTarget,
    binding: NativeBinding,
}

impl LeadTarget {
    pub fn new(target: SessionTarget, binding: NativeBinding) -> Self {
        Self { target, binding }
    }

    pub fn target(&self) -> &SessionTarget {
        &self.target
    }

    pub fn review_id(&self) -> &ReviewId {
        self.target.review_id()
    }

    pub fn run_id(&self) -> &RunId {
        self.target.run_id()
    }

    pub fn session_key(&self) -> &SessionKey {
        self.target.session_key()
    }

    pub fn binding(&self) -> &NativeBinding {
        &self.binding
    }
}
