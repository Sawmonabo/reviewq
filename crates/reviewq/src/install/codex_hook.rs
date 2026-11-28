//! Optional Codex hooks installed alongside the stable user launcher.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::checkout_setup::files::{Existing, read};
use crate::profile_config::{self, Profile, Replacement};

use super::{Failure, Paths, paths};

mod json;

const TIMEOUT_SECONDS: u32 = 5;
const OWNERSHIP: &str = "codex-hooks.json";
const MIXED: &str = "A hook group there runs ReviewQ's command beside other hooks.";

/// A Codex hook event ReviewQ answers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum Event {
    /// The turn is about to end: ReviewQ's gate may keep it open.
    Stop,
    /// Codex compacted the conversation: ReviewQ adds the rule and the ledger list back.
    SessionStart,
}

impl Event {
    /// Every event, in install order. Entries are removed in the reverse order.
    const ALL: [Self; 2] = [Self::Stop, Self::SessionStart];

    /// The event's key in the hooks object.
    const fn name(self) -> &'static str {
        match self {
            Self::Stop => "Stop",
            Self::SessionStart => "SessionStart",
        }
    }

    /// The path from the document to the event's array.
    const fn path(self) -> &'static [&'static str] {
        match self {
            Self::Stop => &["hooks", "Stop"],
            Self::SessionStart => &["hooks", "SessionStart"],
        }
    }

    /// The matcher of ReviewQ's entry: `SessionStart` runs only for the `compact` source.
    const fn matcher(self) -> Option<&'static str> {
        match self {
            Self::Stop => None,
            Self::SessionStart => Some("^compact$"),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    path: PathBuf,
    event: Event,
    command: String,
    added: json::Added,
    applied: bool,
}

/// What `reconcile` did with the Codex hooks.
pub(super) enum Reconciled {
    /// ReviewQ owns exactly the entries it wants.
    Done,
    /// Entries ReviewQ no longer wants are still in these `hooks.json` files, usually because
    /// the user changed them or put ReviewQ's command beside other hooks; `reason` is the last
    /// refusal, and every other entry was reconciled.
    Kept {
        files: Vec<PathBuf>,
        reason: Failure,
    },
}

/// Makes ReviewQ own exactly its `Stop` and `SessionStart` entries in
/// `$CODEX_HOME/hooks.json` when `has_codex`, and none otherwise. An owned entry with another
/// file or command is removed first, so an upgrade replaces ReviewQ's entries; a user's other
/// hooks and Codex's trust state are never touched. The release journal is read by older
/// binaries during rollback, so ownership lives in its own file under the same installation
/// lock.
///
/// The record repairs itself from `hooks.json`. When it is missing or cannot be read, as
/// after a release that wrote another shape, ReviewQ's entries are the ones that run its
/// launcher for this profile: all but the wanted ones are removed by that, under whichever
/// event, and the record starts over. A wanted entry that is recorded but gone is added
/// again, and one that is there but not recorded is taken as ReviewQ's own.
///
/// An entry that cannot be removed, usually because the user changed it, is kept and
/// reported as `Kept` after every other entry was tried; so is a `hooks.json` that cannot be
/// cleared without a record, which leaves the record for the next run. Fails when the
/// ownership file cannot be written or a wanted entry cannot be installed; installation stops
/// at the first entry it cannot write.
pub(super) fn reconcile(paths: &Paths, has_codex: bool) -> Result<Reconciled, Failure> {
    let sidecar = paths.state.join(OWNERSHIP);
    let hooks = paths.codex_home.join("hooks.json");
    let wanted = if has_codex {
        Some((hooks.clone(), command(paths)?))
    } else {
        None
    };
    let mut kept = Vec::new();
    let mut retire_error = None;
    let record = paths::read_json::<Vec<Registration>>(&sidecar);
    let mut owned = if let Ok(Some(owned)) = record {
        owned
    } else {
        let keep = wanted.as_ref().map(|(_, command)| command.as_str());
        match own_prefix(paths).and_then(|prefix| sweep(&hooks, &prefix, keep)) {
            Ok(false) => {}
            Ok(true) => {
                kept.push(hooks.clone());
                retire_error = Some(MIXED.into());
            }
            Err(reason) => {
                return Ok(Reconciled::Kept {
                    files: vec![hooks],
                    reason,
                });
            }
        }
        if record.is_err() {
            fs::remove_file(&sidecar)?;
            paths::sync(&paths.state)?;
        }
        Vec::new()
    };
    let is_wanted = |hook: &Registration| {
        wanted
            .as_ref()
            .is_some_and(|(path, command)| hook.path == *path && hook.command == *command)
    };
    let original_count = owned.len();
    // Newest first, so the entry that created the document removes it last.
    for index in (0..owned.len()).rev() {
        if is_wanted(&owned[index]) {
            continue;
        }
        match owned[index].remove() {
            Ok(()) => {
                owned.remove(index);
            }
            Err(error) => {
                if !kept.contains(&owned[index].path) {
                    kept.push(owned[index].path.clone());
                }
                retire_error = Some(error);
            }
        }
    }
    if owned.len() != original_count {
        if owned.is_empty() {
            fs::remove_file(&sidecar)?;
            paths::sync(&paths.state)?;
        } else {
            paths::write_json(&sidecar, &owned)?;
        }
    }
    if let Some((path, command)) = &wanted {
        for event in Event::ALL {
            let location = owned
                .iter()
                .position(|hook| is_wanted(hook) && hook.event == event);
            // What the file lacks now decides what to add, whatever was recorded.
            let missing = Registration::plan(path, event, command)?;
            let index = match (location, missing) {
                (Some(index), None) => index,
                (Some(index), Some(hook)) => {
                    owned[index] = hook;
                    paths::write_json(&sidecar, &owned)?;
                    index
                }
                (None, missing) => {
                    owned.push(missing.unwrap_or_else(|| Registration {
                        path: path.clone(),
                        event,
                        command: command.clone(),
                        added: json::Added::Matcher,
                        applied: true,
                    }));
                    paths::write_json(&sidecar, &owned)?;
                    owned.len() - 1
                }
            };
            let hook = &mut owned[index];
            hook.install()?;
            if !hook.applied {
                hook.applied = true;
                paths::write_json(&sidecar, &owned)?;
            }
        }
    }
    Ok(match retire_error {
        Some(reason) => Reconciled::Kept {
            files: kept,
            reason,
        },
        None => Reconciled::Done,
    })
}

/// The installed launcher running `internal codex-hook` for the installed profile. `|| :`
/// keeps a missing or older launcher from exiting 2, which Codex reads as a block; stdout
/// stays the hook's answer.
pub(super) fn command(paths: &Paths) -> Result<String, Failure> {
    Ok(format!("{}codex-hook || :", own_prefix(paths)?))
}

/// How every hook command of this installation starts, in any release: the installed
/// launcher running an internal command for the installed profile.
fn own_prefix(paths: &Paths) -> Result<String, Failure> {
    Ok(format!(
        "{} --profile {} internal ",
        shell_path(&paths.launcher)?,
        shell_path(&paths.profile)?,
    ))
}

/// Removes the groups of `hooks` that run only commands starting with `prefix`, except one
/// that runs `keep`. Returns whether a group that runs one beside other hooks was left.
fn sweep(hooks: &Path, prefix: &str, keep: Option<&str>) -> Result<bool, Failure> {
    let mut mixed = false;
    rewrite(hooks, |bytes| {
        let (change, left) = json::sweep(bytes, prefix, keep)?;
        mixed = left;
        Ok(change)
    })?;
    Ok(mixed)
}

/// Replaces `hooks` with what `change` makes of its bytes, keeping its mode. Returns whether
/// the Codex home it belongs in is there.
fn rewrite(
    hooks: &Path,
    mut change: impl FnMut(Option<&[u8]>) -> Result<json::Change, Failure>,
) -> Result<bool, Failure> {
    let replaced = profile_config::replace(hooks, Profile::Codex, |before| {
        let change = change(before.map(|file| file.bytes.as_slice()))?;
        Ok(change.map(|change| {
            let after = change.map(|bytes| Existing {
                bytes,
                mode: before.map_or(0o600, |file| file.mode),
            });
            (after, ())
        }))
    })?;
    match replaced {
        Replacement::Unchanged => Ok(true),
        Replacement::Missing => Ok(false),
        Replacement::Replaced { verified, .. } => {
            verified?;
            profile_config::sync_parent(hooks)?;
            Ok(true)
        }
    }
}

impl Registration {
    fn plan(path: &Path, event: Event, command: &str) -> Result<Option<Self>, Failure> {
        let file = read(path)?;
        let bytes = file.as_ref().map(|file| file.bytes.as_slice());
        let plan = json::plan(bytes, event, command)?;
        Ok(plan.map(|added| Self {
            path: path.to_owned(),
            event,
            command: command.to_owned(),
            added,
            applied: false,
        }))
    }

    fn install(&self) -> Result<(), Failure> {
        self.edit(true)
    }

    fn remove(&self) -> Result<(), Failure> {
        self.edit(false)
    }

    fn edit(&self, installing: bool) -> Result<(), Failure> {
        let present = rewrite(&self.path, |bytes| {
            if installing {
                json::install(bytes, self.event, &self.command, self.added, self.applied)
            } else {
                json::remove(bytes, self.event, &self.command, self.added)
            }
        })?;
        if installing && !present {
            return Err(paths::conflict(&self.path));
        }
        Ok(())
    }
}

fn shell_path(path: &Path) -> Result<String, Failure> {
    let path = path
        .to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .ok_or("Expected a UTF-8 path without control characters for the Codex hook.")?;
    Ok(format!("'{}'", path.replace('\'', "'\\''")))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn hook_lifecycle_keeps_other_entries_modes_and_trust_slots() -> Result<(), Failure> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        let codex_home = home.join("codex home's");
        fs::create_dir(&codex_home)?;
        let paths = Paths {
            home: home.clone(),
            profile: home.join("profile"),
            config_home: home.clone(),
            state_home: home.clone(),
            config: home.join("config"),
            state: home.join("state"),
            launcher: home.join("reviewq dir's/reviewq"),
            unit: home.join("reviewq.service"),
            start_socket: home.join("reviewq-start.socket"),
            codex_home: codex_home.clone(),
            claude_home: home.join("claude"),
            claude_home_is_explicit: false,
        };
        let file = codex_home.join("hooks.json");
        let config = codex_home.join("config.toml");
        let trust = b"[hooks.state.example]\nenabled = false\n";
        fs::write(&config, trust)?;
        fs::create_dir(&paths.state)?;
        let release_journal = paths.state.join("install.json");
        let release_bytes = b"{\"active\":null,\"store_id\":null,\"pending\":null,\"known\":[],\"removing\":false}\n";
        fs::write(&release_journal, release_bytes)?;
        let sidecar = paths.state.join(OWNERSHIP);
        let command = command(&paths)?;

        // A fresh Codex home gets exactly the two entries, and removing them removes the file.
        reconcile(&paths, true)?;
        let created = fs::read(&file)?;
        let doc: serde_json::Value = serde_json::from_slice(&created)?;
        let entry = |matcher: Option<&str>| {
            let mut group = serde_json::json!({
                "hooks": [{"type": "command", "command": command, "timeout": TIMEOUT_SECONDS}],
            });
            if let Some(matcher) = matcher {
                group["matcher"] = matcher.into();
            }
            group
        };
        assert_eq!(
            doc,
            serde_json::json!({"hooks": {
                "Stop": [entry(None)],
                "SessionStart": [entry(Some("^compact$"))],
            }})
        );
        assert!(!command.contains('>') && command.ends_with(" internal codex-hook || :"));
        assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&sidecar)?.permissions().mode() & 0o777, 0o600);
        let owned = paths::read_json::<Vec<Registration>>(&sidecar)?.expect("hook ownership");
        assert_eq!(
            owned
                .iter()
                .map(|hook| (hook.event, hook.applied))
                .collect::<Vec<_>>(),
            [(Event::Stop, true), (Event::SessionStart, true)]
        );
        reconcile(&paths, true)?;
        assert_eq!(fs::read(&file)?, created);
        reconcile(&paths, false)?;
        assert!(!file.try_exists()? && !sidecar.try_exists()?);

        // A user's hooks, number literals and file mode survive install, upgrade and removal.
        let user_stop = r#"{"hooks":[{"type":"command","command":"/bin/false"}]}"#;
        let later = r#"{"matcher":"^Edit$","hooks":[{"type":"command","command":"/bin/true"}]}"#;
        let original = format!(
            "{{ \"metadata\": 0.123456789012345678901, \"hooks\" : {{ \"PreToolUse\" : [ {later} ], \"Stop\" : [ {user_stop} ] }} }}\n"
        );
        fs::write(&file, &original)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640))?;
        reconcile(&paths, true)?;
        let upgraded = Paths {
            launcher: home.join("other/reviewq"),
            ..paths.clone()
        };
        reconcile(&upgraded, true)?;
        let with_hooks = fs::read_to_string(&file)?;
        let doc: serde_json::Value = serde_json::from_str(&with_hooks)?;
        let upgraded_command = super::command(&upgraded)?;
        assert_eq!(
            doc["hooks"]["Stop"][1]["hooks"][0]["command"],
            upgraded_command
        );
        assert_eq!(
            doc["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            upgraded_command
        );
        assert!(!with_hooks.contains(&command) && with_hooks.contains("0.123456789012345678901"));
        assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o777, 0o640);

        // An entry the user edited is preserved and reported with its file.
        let edited = with_hooks.replace("\"timeout\":5", "\"timeout\":7");
        fs::write(&file, &edited)?;
        assert!(matches!(
            reconcile(&upgraded, false)?,
            Reconciled::Kept { files, .. } if files == [file.clone()]
        ));
        assert_eq!(fs::read_to_string(&file)?, edited);

        // A later user entry keeps its slot, which Codex's trust keys name.
        let owned_stop = serde_json::to_string(&doc["hooks"]["Stop"][1])?;
        fs::write(
            &file,
            with_hooks.replace(&owned_stop, &format!("{owned_stop},{later}")),
        )?;
        reconcile(&upgraded, false)?;
        assert_eq!(
            fs::read_to_string(&file)?,
            original.replace(user_stop, &format!(r#"{user_stop},{{"hooks":[]}},{later}"#)),
        );
        assert!(!sidecar.try_exists()?);
        assert_eq!(fs::read(&release_journal)?, release_bytes);
        assert_eq!(fs::read(&config)?, trust);
        assert!(!file.with_extension("json.lock").try_exists()?);

        // A record this build cannot read: ReviewQ's entries go by the command they run,
        // under whichever event an earlier release used, and the record starts over.
        let stale = serde_json::json!({"matcher": "^old$", "hooks": [{"type": "command",
            "command": format!("{}old-witness || :", own_prefix(&upgraded)?), "timeout": 5}]});
        let slots = format!(r#""Stop":[{user_stop},{owned_stop},{later}]"#);
        fs::write(
            &file,
            format!(r#"{{"hooks":{{"PreToolUse":[{stale},{later},{stale}],{slots}}}}}"#),
        )?;
        fs::write(&sidecar, br#"[{"path":"hooks.json"}]"#)?;
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600))?;
        assert!(matches!(reconcile(&upgraded, true)?, Reconciled::Done));
        let healed = fs::read_to_string(&file)?;
        assert!(healed.contains(&format!(r#""PreToolUse":[{{"hooks":[]}},{later}]"#)));
        assert!(healed.contains(&slots));
        let installed = |text: &str| -> Result<bool, Failure> {
            let doc: serde_json::Value = serde_json::from_str(text)?;
            Ok(
                doc["hooks"]["Stop"][1]["hooks"][0]["command"] == upgraded_command
                    && doc["hooks"]["SessionStart"][0]["hooks"][0]["command"] == upgraded_command,
            )
        };
        assert!(installed(&healed)?);
        let recorded = || paths::read_json::<Vec<Registration>>(&sidecar);
        assert_eq!(recorded()?.map(|owned| owned.len()), Some(2));

        // An entry removed behind ReviewQ's back comes back. Without a record, an entry that
        // is there is taken as ReviewQ's own and an earlier release's goes.
        fs::write(&file, format!(r#"{{"hooks":{{"Stop":[{user_stop}]}}}}"#))?;
        reconcile(&upgraded, true)?;
        let current = fs::read_to_string(&file)?;
        assert!(installed(&current)?);
        fs::remove_file(&sidecar)?;
        let earlier = format!(r#"{{"hooks":{{"PreToolUse":[{stale}],"#);
        fs::write(&file, current.replacen(r#"{"hooks":{"#, &earlier, 1))?;
        reconcile(&upgraded, true)?;
        assert_eq!(recorded()?.map(|owned| owned.len()), Some(2));
        let adopted = fs::read_to_string(&file)?;
        assert!(installed(&adopted)? && !adopted.contains("old-witness"));
        reconcile(&upgraded, false)?;
        assert!(!fs::read_to_string(&file)?.contains(&upgraded_command));

        // Hooks that cannot be cleared without a record are reported, and the record stays
        // for the next run.
        fs::write(&file, "not JSON")?;
        fs::write(&sidecar, br#"[{"path":"hooks.json"}]"#)?;
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600))?;
        assert!(matches!(
            reconcile(&upgraded, false)?,
            Reconciled::Kept { files, .. } if files == [file.clone()]
        ));
        assert!(sidecar.try_exists()?);
        Ok(())
    }
}
