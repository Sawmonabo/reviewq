//! The daemon session a reply comes from.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde_json::Value;

/// One daemon session of a connection, as reading a conversation's hooks from it needs.
/// Every reply comes with the session that answered it, so a reply is judged only by what
/// that same daemon showed.
pub(in crate::codex) struct Served {
    /// `hooks.json` in the daemon's Codex home, named as the daemon names its hooks.
    pub(in crate::codex) hooks_file: PathBuf,
    /// Whether this daemon lists one conversation's hooks, once a hook check on this session
    /// asked it (`hook_reload::scope_probe`). Connecting does not ask.
    scoped: OnceLock<bool>,
}

impl Served {
    pub(super) fn new(hooks_file: PathBuf) -> Self {
        Self {
            hooks_file,
            scoped: OnceLock::new(),
        }
    }

    /// Whether this daemon lists one conversation's hooks, when a check on this session
    /// has learned it.
    pub(in crate::codex) fn thread_scoped(&self) -> Option<bool> {
        self.scoped.get().copied()
    }

    /// Keeps what the daemon of this session answered to `hook_reload::scope_probe`.
    pub(in crate::codex) fn learn_scope(&self, scoped: bool) {
        let _ = self.scoped.set(scoped);
    }
}

/// A raw reply frame with the session that answered it.
pub(in crate::codex) type Reply = (Value, Arc<Served>);
