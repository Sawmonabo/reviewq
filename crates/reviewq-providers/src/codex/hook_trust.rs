//! Trusts ReviewQ's own Codex hooks the way Codex's `/hooks` screen does, and removes that
//! trust again: a private `codex app-server` on stdio lists the hooks, writes `hooks.state`
//! for ReviewQ's hooks only and lists them again to see the change.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use reviewq_app::access::AppError;
use reviewq_app::hosts::HookApproval;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use super::Codex;
use super::connection::{FRAME_LIMIT, accept_initialize, initialize_params, rpc_result};

const DEADLINE: Duration = Duration::from_secs(10);
const EXIT_GRACE: Duration = Duration::from_secs(1);

/// One entry of `hooks/list`, as far as choosing ReviewQ's hooks needs it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hook {
    key: String,
    source_path: Option<PathBuf>,
    source: Option<String>,
    handler_type: Option<String>,
    command: Option<String>,
    is_managed: Option<bool>,
    current_hash: Option<String>,
    trust_status: Option<String>,
}

#[derive(Deserialize)]
struct Listed {
    data: Vec<DirectoryHooks>,
}

/// The hooks `hooks/list` reports for one working directory.
#[derive(Deserialize)]
struct DirectoryHooks {
    hooks: Vec<Hook>,
}

/// What `revoke_hooks` did with Codex's trust in ReviewQ's own hooks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookRevocation {
    /// Codex no longer holds a trust entry for this many of them.
    Revoked(usize),
    /// Codex holds no trust entry for any of them.
    NoneTrusted,
    /// Codex lists none of them.
    NotListed,
    /// This Codex cannot say which hooks it trusts.
    Unsupported,
}

/// ReviewQ's own hooks among those Codex lists, by what each needs.
#[derive(Debug, Default, Eq, PartialEq)]
struct Own {
    listed: bool,
    /// Untrusted or modified: each key with the hash to trust it at.
    untrusted: Map<String, Value>,
    /// Trusted or modified: the keys Codex holds a trust entry for.
    recorded: Vec<String>,
}

/// The last completed check, with the state of the files that decide it when it ended.
pub(super) type LastCheck = tokio::sync::Mutex<Option<(Stamps, HookApproval)>>;

/// Modification time and length of the profile's `hooks.json` and `config.toml` and of the
/// Codex executable; `None` for a file that cannot be read.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct Stamps([Option<(SystemTime, u64)>; 3]);

impl Stamps {
    fn read(executable: &Path, profile: &Path) -> Self {
        let stamp = |path: &Path| {
            let metadata = std::fs::metadata(path).ok()?;
            Some((metadata.modified().ok()?, metadata.len()))
        };
        Self([
            stamp(&profile.join("hooks.json")),
            stamp(&profile.join("config.toml")),
            stamp(executable),
        ])
    }
}

impl Codex {
    /// Trusts ReviewQ's own hooks in this profile and returns what Codex says of them. A
    /// Codex child starts only when `hooks.json`, `config.toml` or the executable changed
    /// since the last completed check; otherwise, and for callers that waited behind a
    /// running check, the remembered outcome returns at once. A failure is logged and not
    /// remembered. Without a hook command, as in a service other than the installed one,
    /// nothing is ReviewQ's to trust.
    pub(super) async fn approve_own_hooks(&self) -> Result<HookApproval, AppError> {
        let Some(command) = &self.hook_command else {
            return Ok(HookApproval::NotListed);
        };
        let profile = self.profile.as_path();
        let mut last = self.last_hook_check.lock().await;
        let stamps = Stamps::read(&self.executable, profile);
        if let Some((seen, outcome)) = *last
            && seen == stamps
        {
            return Ok(outcome);
        }
        let outcome = approve_hooks(&self.executable, profile, command).await;
        // Read after the exchange, since trusting a hook changes `config.toml`.
        *last = outcome
            .as_ref()
            .ok()
            .map(|outcome| (Stamps::read(&self.executable, profile), *outcome));
        match &outcome {
            Ok(HookApproval::Approved(count)) => {
                eprintln!("ReviewQ approved {count} of its Codex hooks.");
            }
            Ok(_) => {}
            Err(error) => eprintln!("ReviewQ could not approve its Codex hooks ({error})."),
        }
        outcome
    }
}

/// Starts `executable app-server` for the Codex home `profile` and trusts the hooks there
/// that are ReviewQ's own: `profile/hooks.json` user command hooks that run exactly
/// `command`. Reports success only once Codex lists each of them as trusted. Fails when the
/// server cannot be started or answers out of turn, refuses the write, or the exchange
/// outlasts its deadline; the server is stopped and reaped either way.
pub async fn approve_hooks(
    executable: &Path,
    profile: &Path,
    command: &str,
) -> Result<HookApproval, AppError> {
    exchange(executable, profile, async |server, hooks_file| {
        let Some(own) = own_hooks(&server.list().await?, hooks_file, command) else {
            return Ok(HookApproval::Unsupported);
        };
        if !own.listed {
            return Ok(HookApproval::NotListed);
        }
        if own.untrusted.is_empty() {
            return Ok(HookApproval::AlreadyTrusted);
        }
        let keys = own.untrusted.keys().cloned().collect::<Vec<_>>();
        let edit = json!({"keyPath":"hooks.state","value":own.untrusted,"mergeStrategy":"upsert"});
        server.write(vec![edit]).await?;
        server
            .confirm(&keys, hooks_file, command, "trusted")
            .await?;
        Ok(HookApproval::Approved(keys.len()))
    })
    .await
}

/// Removes Codex's trust entries for ReviewQ's own hooks in `profile`, chosen as
/// `approve_hooks` chooses them, while they are still listed, so a later hook in the same
/// place starts untrusted. Codex removes an entry when its key is written as `null`. Reports
/// success only once Codex lists each of them as untrusted, and fails as `approve_hooks` does.
pub async fn revoke_hooks(
    executable: &Path,
    profile: &Path,
    command: &str,
) -> Result<HookRevocation, AppError> {
    exchange(executable, profile, async |server, hooks_file| {
        let Some(own) = own_hooks(&server.list().await?, hooks_file, command) else {
            return Ok(HookRevocation::Unsupported);
        };
        if !own.listed {
            return Ok(HookRevocation::NotListed);
        }
        if own.recorded.is_empty() {
            return Ok(HookRevocation::NoneTrusted);
        }
        let edits = own
            .recorded
            .iter()
            .map(|key| json!({"keyPath":state_path(key),"value":null,"mergeStrategy":"replace"}))
            .collect();
        server.write(edits).await?;
        server
            .confirm(&own.recorded, hooks_file, command, "untrusted")
            .await?;
        Ok(HookRevocation::Revoked(own.recorded.len()))
    })
    .await
}

/// Runs `work` against a private `executable app-server` for the Codex home `profile` once
/// it has answered `initialize` for that home, within the deadline. `work` gets the server and
/// the `hooks.json` path Codex names its hooks by.
///
/// Codex names hooks by its home as it resolves it: the real path of `CODEX_HOME` when that
/// is set, and `$HOME/.codex` as written otherwise. When `$HOME/.codex` leads to `profile`,
/// the child derives its home the same way, so a symlinked home gets the keys the user's
/// Codex looks up.
async fn exchange<T>(
    executable: &Path,
    profile: &Path,
    work: impl AsyncFnOnce(&mut Server, &Path) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let profile = profile.canonicalize().map_err(|_| AppError::Unavailable)?;
    let mut command = Command::new(executable);
    let default_home = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"));
    if default_home.and_then(|home| home.canonicalize().ok()) == Some(profile.clone()) {
        command.env_remove("CODEX_HOME");
    } else {
        command.env("CODEX_HOME", &profile);
    }
    let mut child = command
        .arg("app-server")
        .current_dir(Path::new("/"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| AppError::Unavailable)?;
    let outcome = match (child.stdin.take(), child.stdout.take()) {
        (Some(input), Some(output)) => {
            let mut server = Server {
                input,
                output: BufReader::new(output),
                id: 0,
            };
            timeout(DEADLINE, async {
                let result = server.call("initialize", initialize_params()).await?;
                let (home, initialized) = accept_initialize(result, &profile)?;
                server.send(initialized).await?;
                work(&mut server, &home.join("hooks.json")).await
            })
            .await
            .unwrap_or(Err(AppError::Unavailable))
        }
        _ => Err(AppError::Unavailable),
    };
    // Closing stdin ends the server; one that outlives the grace is killed and reaped.
    if timeout(EXIT_GRACE, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
    outcome
}

/// The `config/batchWrite` key path of the trust entry for the hook `key`, which holds dots.
fn state_path(key: &str) -> String {
    format!(
        "hooks.state.\"{}\"",
        key.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

struct Server {
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    id: u64,
}

impl Server {
    async fn write(&mut self, edits: Vec<Value>) -> Result<(), AppError> {
        self.call("config/batchWrite", json!({"edits":edits}))
            .await
            .map(|_| ())
    }

    /// Fails unless Codex now lists each of `keys` as ReviewQ's own hook with `status`.
    async fn confirm(
        &mut self,
        keys: &[String],
        hooks_file: &Path,
        command: &str,
        status: &str,
    ) -> Result<(), AppError> {
        let listed = self.list().await?;
        let settled = |key: &String| {
            listed.iter().any(|hook| {
                hook.key == *key
                    && is_own(hook, hooks_file, command)
                    && hook.trust_status.as_deref() == Some(status)
            })
        };
        if !keys.iter().all(settled) {
            return Err(AppError::Unavailable);
        }
        Ok(())
    }

    async fn list(&mut self) -> Result<Vec<Hook>, AppError> {
        let listed: Listed = serde_json::from_value(self.call("hooks/list", json!({})).await?)
            .map_err(|_| AppError::Unavailable)?;
        Ok(listed
            .data
            .into_iter()
            .flat_map(|layer| layer.hooks)
            .collect())
    }

    /// Sends one request and returns its reply, skipping notifications and server requests.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, AppError> {
        self.id += 1;
        self.send(json!({"id":self.id,"method":method,"params":params}))
            .await?;
        loop {
            let frame = self.receive().await?;
            if frame.get("id").and_then(Value::as_u64) == Some(self.id)
                && frame.get("method").is_none()
            {
                return rpc_result(frame);
            }
        }
    }

    async fn send(&mut self, message: Value) -> Result<(), AppError> {
        let mut line = message.to_string();
        line.push('\n');
        self.input
            .write_all(line.as_bytes())
            .await
            .map_err(|_| AppError::Unavailable)?;
        self.input.flush().await.map_err(|_| AppError::Unavailable)
    }

    /// Reads one line of at most `FRAME_LIMIT` bytes. A longer line, or one the server ends
    /// without a newline, fails.
    async fn receive(&mut self) -> Result<Value, AppError> {
        let mut line = Vec::new();
        (&mut self.output)
            .take(FRAME_LIMIT as u64 + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if line.pop() != Some(b'\n') {
            return Err(AppError::Unavailable);
        }
        serde_json::from_slice(&line).map_err(|_| AppError::Unavailable)
    }
}

/// Sorts ReviewQ's own hooks among `hooks` by what each needs. `None` when Codex leaves out
/// the trust status or hash of one of them, as before 0.129.0.
fn own_hooks(hooks: &[Hook], hooks_file: &Path, command: &str) -> Option<Own> {
    let mut own = Own::default();
    for hook in hooks
        .iter()
        .filter(|hook| is_own(hook, hooks_file, command))
    {
        own.listed = true;
        let status = hook.trust_status.as_deref();
        if matches!(status, Some("trusted" | "modified")) {
            own.recorded.push(hook.key.clone());
        }
        match (status, &hook.current_hash) {
            (Some("trusted"), _) => {}
            (Some("untrusted" | "modified"), Some(hash)) => {
                own.untrusted
                    .insert(hook.key.clone(), json!({"trusted_hash": hash}));
            }
            _ => return None,
        }
    }
    own.recorded.sort();
    own.recorded.dedup();
    Some(own)
}

/// Whether `hook` is one ReviewQ installed: an unmanaged user command hook in `hooks_file`
/// that runs exactly `command`.
fn is_own(hook: &Hook, hooks_file: &Path, command: &str) -> bool {
    hook.source_path.as_deref() == Some(hooks_file)
        && hook.source.as_deref() == Some("user")
        && hook.handler_type.as_deref() == Some("command")
        && hook.is_managed == Some(false)
        && hook.command.as_deref() == Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_only_reviewqs_user_hooks() {
        let file = Path::new("/home/u/.codex/hooks.json");
        let ours = "'/home/u/.local/bin/reviewq' --profile '/p' internal codex-hook || :";
        let hook = |key: &str, command: &str, source: &str, managed: bool, trust: &str| {
            serde_json::from_value::<Hook>(json!({
                "key": key, "eventName": "stop", "matcher": null,
                "sourcePath": file, "source": source, "handlerType": "command",
                "command": command, "enabled": true, "isManaged": managed,
                "currentHash": format!("sha256:{key}"), "trustStatus": trust,
            }))
            .unwrap()
        };
        let hooks = [
            hook("stop", ours, "user", false, "untrusted"),
            hook("start", ours, "user", false, "trusted"),
            hook("other", "/bin/other-tool", "user", false, "untrusted"),
            hook("project", ours, "project", false, "untrusted"),
            hook("managed", ours, "user", true, "untrusted"),
        ];
        let mut untrusted = Map::new();
        untrusted.insert("stop".into(), json!({"trusted_hash": "sha256:stop"}));
        let own = Own {
            listed: true,
            untrusted,
            recorded: vec!["start".into()],
        };
        assert_eq!(own_hooks(&hooks, file, ours), Some(own));

        let mut older = hook("stop", ours, "user", false, "untrusted");
        older.current_hash = None;
        assert_eq!(own_hooks(&[older], file, ours), None);
    }
}
