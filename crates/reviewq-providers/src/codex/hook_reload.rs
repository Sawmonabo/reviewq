//! Brings a change in Codex's trust of ReviewQ's hooks into the conversations Codex already
//! has open, and reads whether one of them runs ReviewQ's hooks now.
//!
//! Codex builds a conversation's hooks once, when it loads the conversation, and a trust entry
//! another process writes does not reach it. The app-server that hosts the conversation
//! rebuilds the hooks of every conversation it holds when asked to reload the user config, as
//! Codex's own `/hooks` screen asks after a trust change. Its `ok` says nothing of the result,
//! so only that conversation's own hook list shows whether the hooks are live there. ReviewQ
//! reaches only the user's shared daemon; a conversation Codex runs in a process of its own
//! keeps its hooks until Codex loads it again.

use std::path::Path;
use std::time::Duration;

use reviewq_app::access::AppError;
use reviewq_app::hosts::LiveHooks;
use reviewq_domain::identity::NativeSessionId;
use serde::de::IgnoredAny;
use serde_json::{Value, json};
use tokio::time::timeout;

use super::Codex;
use super::connection::{Served, request_once};
use super::hook_trust::{Hook, Listed, is_own};

/// How long the reload after a trust change may take, discovery and handshake included.
const RELOAD_DEADLINE: Duration = Duration::from_secs(2);
/// How long the check of a conversation's hooks at link may take, its one reload included.
const CHECK_DEADLINE: Duration = Duration::from_secs(3);

/// The request that makes an app-server rebuild the hooks of every conversation it holds. It
/// writes nothing; it also re-reads the rest of the user config for those conversations.
fn reload_request() -> (&'static str, Value) {
    (
        "config/batchWrite",
        json!({"edits":[],"reloadUserConfig":true}),
    )
}

/// Asks the running Codex daemon of the Codex home `profile` to reload hook trust into the
/// conversations it holds, once, right after ReviewQ changed that trust. Starts no daemon:
/// with none running there is nothing to reload. A failure only leaves those conversations
/// as they were, so it is ignored.
pub(super) async fn reload_open_conversations(executable: &Path, profile: &Path) {
    let Ok(profile) = profile.canonicalize() else {
        return;
    };
    let (method, params) = reload_request();
    let _ = timeout(
        RELOAD_DEADLINE,
        request_once(executable, &profile, method, params),
    )
    .await;
}

/// The request that asks a new daemon session whether `hooks/list` honours `threadId`: the
/// hooks of a conversation that cannot exist. A Codex that scopes the list by conversation
/// answers that it has no such thread; one that ignores the field, as before 0.166.0, lists
/// the hooks on disk, which say nothing of any conversation.
pub(super) fn scope_probe() -> (&'static str, Value) {
    let thread = uuid::Uuid::new_v4().to_string();
    ("hooks/list", json!({"threadId": thread}))
}

/// Whether the reply to `scope_probe` shows a daemon that lists one conversation's hooks.
/// Only the refusal Codex gives for an unknown thread counts; any other answer leaves every
/// later reading unconfirmed. A Codex with no `hooks/list` refuses the same way, but then
/// lists no conversation's hooks either, so its readings stay unconfirmed too.
pub(super) fn scopes_by_thread(reply: &Result<Value, AppError>) -> bool {
    matches!(reply, Err(AppError::NotFound))
}

/// Whether one conversation runs ReviewQ's hooks now, as far as its app-server says.
#[derive(Debug, Eq, PartialEq)]
enum ThreadHooks {
    /// The conversation holds each of ReviewQ's hooks, trusted.
    Live,
    /// The daemon holds the conversation, but not ReviewQ's hooks trusted.
    Stale,
    /// The daemon does not hold the conversation, cannot list one conversation's hooks, or
    /// answered otherwise.
    Unknown,
}

impl Codex {
    /// What the daemon shows of ReviewQ's hooks in the open conversation `thread`. When it
    /// holds the conversation without them, it is asked once to reload them, and asked again.
    /// `Unconfirmed` whenever it cannot show either within the deadline.
    pub(super) async fn own_hooks_live(&self, thread: &NativeSessionId) -> LiveHooks {
        let Some(command) = self.hook_command.as_deref() else {
            return LiveHooks::Unconfirmed;
        };
        let check = async {
            match self.thread_hooks(thread, command).await {
                ThreadHooks::Live => return LiveHooks::Running,
                ThreadHooks::Unknown => return LiveHooks::Unconfirmed,
                ThreadHooks::Stale => {}
            }
            let (method, params) = reload_request();
            if self
                .connection
                .read::<IgnoredAny>(method, params)
                .await
                .is_err()
            {
                return LiveHooks::Unconfirmed;
            }
            match self.thread_hooks(thread, command).await {
                ThreadHooks::Live => LiveHooks::Running,
                ThreadHooks::Stale => LiveHooks::NotRunning,
                ThreadHooks::Unknown => LiveHooks::Unconfirmed,
            }
        };
        timeout(CHECK_DEADLINE, check)
            .await
            .unwrap_or(LiveHooks::Unconfirmed)
    }

    /// Reads the hooks the daemon holds for `thread`: one request on the service's connection.
    async fn thread_hooks(&self, thread: &NativeSessionId, command: &str) -> ThreadHooks {
        let listed = self
            .connection
            .read::<Listed>("hooks/list", json!({"threadId": thread.as_str()}))
            .await;
        judge(
            listed.ok().map(Listed::hooks),
            self.connection.served().as_ref(),
            command,
        )
    }
}

/// Sorts a conversation's `hooks/list` reply, `None` when it failed, as "thread not found"
/// does for a conversation the daemon does not hold.
fn judge(hooks: Option<Vec<Hook>>, served: Option<&Served>, command: &str) -> ThreadHooks {
    let (Some(hooks), Some(served)) = (hooks, served) else {
        return ThreadHooks::Unknown;
    };
    if !served.thread_scoped {
        return ThreadHooks::Unknown;
    }
    let mut own = hooks
        .iter()
        .filter(|hook| is_own(hook, &served.hooks_file, command))
        .peekable();
    if own.peek().is_none() {
        return ThreadHooks::Stale;
    }
    if own.all(|hook| hook.trust_status.as_deref() == Some("trusted")) {
        ThreadHooks::Live
    } else {
        ThreadHooks::Stale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Codex before 0.166.0 ignores `threadId` and answers with the trusted hooks on disk,
    /// which must not read as live in a conversation opened before they were trusted.
    #[test]
    fn only_a_thread_scoped_listing_reads_as_live() {
        let file = Path::new("/home/u/.codex/hooks.json");
        let ours = "'/home/u/.local/bin/reviewq' --profile '/p' internal codex-hook || :";
        let hooks = |trust: &str| {
            let listed: Listed = serde_json::from_value(json!({"data": [{"cwd": "/", "hooks": [{
                "key": "k", "sourcePath": file, "source": "user", "handlerType": "command",
                "command": ours, "isManaged": false, "trustStatus": trust,
            }]}]}))
            .unwrap();
            Some(listed.hooks())
        };
        let probed = |reply: Result<Value, AppError>| Served {
            hooks_file: file.into(),
            thread_scoped: scopes_by_thread(&reply),
        };
        let current = probed(Err(AppError::NotFound));
        assert_eq!(
            judge(hooks("trusted"), Some(&current), ours),
            ThreadHooks::Live
        );
        assert_eq!(
            judge(hooks("untrusted"), Some(&current), ours),
            ThreadHooks::Stale
        );
        assert_eq!(
            judge(Some(Vec::new()), Some(&current), ours),
            ThreadHooks::Stale
        );
        assert_eq!(judge(None, Some(&current), ours), ThreadHooks::Unknown);
        let older = probed(Ok(json!({"data": []})));
        assert_eq!(
            judge(hooks("trusted"), Some(&older), ours),
            ThreadHooks::Unknown
        );
        let unanswered = probed(Err(AppError::Unavailable));
        assert_eq!(
            judge(hooks("trusted"), Some(&unanswered), ours),
            ThreadHooks::Unknown
        );
    }
}
