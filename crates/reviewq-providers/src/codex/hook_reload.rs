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
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reviewq_app::hosts::LiveHooks;
use reviewq_domain::identity::NativeSessionId;
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

use super::Codex;
use super::connection::{Served, request_once, rpc_result};
use super::hook_trust::{Hook, Listed, is_own};

/// How long the reload after a trust change may take, discovery and handshake included.
const RELOAD_DEADLINE: Duration = Duration::from_secs(2);
/// How long the check of a conversation's hooks at link may take, its connection and one
/// reload included.
const CHECK_DEADLINE: Duration = Duration::from_secs(3);
/// The pauses before each new list of a conversation's hooks after the reload, in case Codex
/// applies it a moment after it replies: about 1.3 seconds in all. The first list follows the
/// reply at once. A slow connection can leave `CHECK_DEADLINE` too little for all of them;
/// then the last list seen after the reload decides.
const RELIST_PAUSES: [Duration; 7] = [
    Duration::ZERO,
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
    Duration::from_millis(160),
    Duration::from_millis(320),
    Duration::from_millis(640),
];

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

/// The request that asks a daemon session whether `hooks/list` honours `threadId`: the hooks
/// of a conversation that cannot exist. A Codex that scopes the list by conversation, 0.166.0
/// and later, answers that it has no such thread; an older one ignores the field and lists
/// the hooks on disk, which say nothing of any conversation.
fn scope_probe() -> (&'static str, Value) {
    let thread = uuid::Uuid::new_v4().to_string();
    ("hooks/list", json!({"threadId": thread}))
}

/// Whether the reply frame to `scope_probe` shows a daemon that lists one conversation's
/// hooks. Only the refusal Codex gives for an unknown thread counts: an invalid-request error
/// whose message starts "thread not found", as every Codex from 0.166.0 words it. Any other
/// answer, such as the same error code for an unknown method, leaves every reading on that
/// session unconfirmed.
fn scopes_by_thread(reply: &Value) -> bool {
    reply.pointer("/error/code").and_then(Value::as_i64) == Some(-32600)
        && reply
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.starts_with("thread not found"))
}

/// What one conversation's app-server shows of ReviewQ's hooks there.
#[derive(Debug, Eq, PartialEq)]
enum ThreadHooks {
    /// The conversation holds each of ReviewQ's hooks, trusted.
    Live,
    /// The daemon holds the conversation, but not ReviewQ's hooks trusted.
    Stale,
}

impl Codex {
    /// What the daemon shows of ReviewQ's hooks in the open conversation `thread`. When it
    /// holds the conversation without them, it is asked once to reload them and listed again
    /// a few times. When the deadline ends the lists, the last one after the reload stands.
    /// `Unconfirmed` whenever it cannot show either, saying whether the conversation was
    /// created before ReviewQ last wrote Codex's trust in them.
    pub(super) async fn own_hooks_live(&self, thread: &NativeSessionId) -> LiveHooks {
        let mut shown = None;
        if let Some(command) = self.hook_command.as_deref() {
            let _ = timeout(
                CHECK_DEADLINE,
                self.shown_hooks(thread, command, &mut shown),
            )
            .await;
        }
        shown.unwrap_or_else(|| LiveHooks::Unconfirmed {
            predates_trust: self.predates_trust(thread),
        })
    }

    /// Sets `shown` to `Running` or `NotRunning` as one daemon session shows the conversation,
    /// from its first listing, its reload and each later listing alike, so a deadline that
    /// cuts the listings short keeps what the last one showed. Clears it whenever a listing
    /// fails or comes from another session.
    async fn shown_hooks(
        &self,
        thread: &NativeSessionId,
        command: &str,
        shown: &mut Option<LiveHooks>,
    ) {
        let Some((first, session)) = self.thread_hooks(thread, command).await else {
            return;
        };
        if first == ThreadHooks::Live {
            *shown = Some(LiveHooks::Running);
            return;
        }
        let (method, params) = reload_request();
        let Ok((reply, reloaded)) = self.connection.request(method, params).await else {
            return;
        };
        if rpc_result(reply).is_err() || !Arc::ptr_eq(&reloaded, &session) {
            return;
        }
        for pause in RELIST_PAUSES {
            sleep(pause).await;
            *shown = match self.thread_hooks(thread, command).await {
                Some((ThreadHooks::Live, listed)) if Arc::ptr_eq(&listed, &session) => {
                    Some(LiveHooks::Running)
                }
                Some((ThreadHooks::Stale, listed)) if Arc::ptr_eq(&listed, &session) => {
                    Some(LiveHooks::NotRunning)
                }
                _ => None,
            };
            if *shown != Some(LiveHooks::NotRunning) {
                return;
            }
        }
    }

    /// Reads the hooks the daemon holds for `thread`, with the session that answered. The
    /// first check on a session also asks, beside the list, whether its daemon lists one
    /// conversation's hooks, and keeps the answer for that session. `None` when the list
    /// failed, as "thread not found" does for a conversation the daemon does not hold, or
    /// that session cannot list one conversation's hooks.
    async fn thread_hooks(
        &self,
        thread: &NativeSessionId,
        command: &str,
    ) -> Option<(ThreadHooks, Arc<Served>)> {
        let list = self
            .connection
            .request("hooks/list", json!({"threadId": thread.as_str()}));
        let learned = self
            .connection
            .served()
            .is_some_and(|session| session.thread_scoped().is_some());
        let listed = if learned {
            list.await
        } else {
            let (method, params) = scope_probe();
            let (listed, probed) = tokio::join!(list, self.connection.request(method, params));
            if let Ok((reply, session)) = probed {
                session.learn_scope(scopes_by_thread(&reply));
            }
            listed
        };
        let (reply, session) = listed.ok()?;
        let listed: Listed = serde_json::from_value(rpc_result(reply).ok()?).ok()?;
        let hooks = listed.hooks();
        let judged = judge(
            &hooks,
            &session.hooks_file,
            session.thread_scoped()?,
            command,
        )?;
        Some((judged, session))
    }

    /// Whether the conversation `thread` was created before ReviewQ last wrote Codex's trust
    /// in its hooks. False whenever either time is unknown. The creation time stands in for
    /// when Codex opened the conversation, which Codex does not say.
    fn predates_trust(&self, thread: &NativeSessionId) -> bool {
        match (created_at(thread.as_str()), self.trusted_at()) {
            (Some(created), Some(trusted)) => created < trusted,
            _ => false,
        }
    }
}

/// When Codex created the conversation `id`, read from the ID itself. Codex makes a thread ID
/// as a UUID version 7 (`ThreadId::new` calls `Uuid::now_v7` in `codex-rs/protocol`), whose
/// first 48 bits are the Unix time in milliseconds. `None` for an ID of any other version.
fn created_at(id: &str) -> Option<SystemTime> {
    let uuid = uuid::Uuid::parse_str(id).ok()?;
    if uuid.get_version_num() != 7 {
        return None;
    }
    let bytes = uuid.as_bytes();
    let mut millis = [0; 8];
    millis[2..].copy_from_slice(&bytes[..6]);
    UNIX_EPOCH.checked_add(Duration::from_millis(u64::from_be_bytes(millis)))
}

/// Sorts a conversation's hook list from a daemon session that `scoped` says lists one
/// conversation's hooks; `None` from one that does not, whose list says nothing of it.
fn judge(hooks: &[Hook], hooks_file: &Path, scoped: bool, command: &str) -> Option<ThreadHooks> {
    if !scoped {
        return None;
    }
    let mut own = hooks
        .iter()
        .filter(|hook| is_own(hook, hooks_file, command))
        .peekable();
    if own.peek().is_none() {
        return Some(ThreadHooks::Stale);
    }
    Some(
        if own.all(|hook| hook.trust_status.as_deref() == Some("trusted")) {
            ThreadHooks::Live
        } else {
            ThreadHooks::Stale
        },
    )
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
            listed.hooks()
        };
        let refusal =
            |message: &str| json!({"id": 3, "error": {"code": -32600, "message": message}});
        let current = scopes_by_thread(&refusal(
            "thread not found: 6ebe8b3f-39f3-41d9-9326-505deb43262c",
        ));
        assert!(current);
        assert_eq!(
            judge(&hooks("trusted"), file, current, ours),
            Some(ThreadHooks::Live)
        );
        assert_eq!(
            judge(&hooks("untrusted"), file, current, ours),
            Some(ThreadHooks::Stale)
        );
        assert_eq!(judge(&[], file, current, ours), Some(ThreadHooks::Stale));
        // An older Codex lists the hooks on disk; one without `hooks/list` refuses the method
        // with the same code.
        let older = scopes_by_thread(&json!({"id": 3, "result": {"data": []}}));
        let unknown_method =
            scopes_by_thread(&refusal("Invalid request: unknown variant `hooks/list`"));
        assert!(!older && !unknown_method);
        assert_eq!(judge(&hooks("trusted"), file, older, ours), None);
    }

    /// A Codex thread ID carries its creation time only as a UUID version 7.
    #[test]
    fn reads_creation_time_only_from_a_version_7_thread_id() {
        let created = created_at("01a22120-e31b-7c83-bb3f-0aac8462e053");
        assert_eq!(
            created,
            UNIX_EPOCH.checked_add(Duration::from_millis(1_795_852_133_147))
        );
        assert_eq!(created_at("6ebe8b3f-39f3-41d9-9326-505deb43262c"), None);
    }
}
