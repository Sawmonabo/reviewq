//! Optional Codex hooks installed alongside the stable user launcher.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::checkout_setup::files::{Existing, read};
use crate::profile_config::{self, Lock, Profile};

use super::{Failure, Paths, paths};

mod json;

const TIMEOUT_SECONDS: u32 = 5;
const OWNERSHIP: &str = "codex-hooks.json";

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
    /// The ownership file at this path cannot be read, so nothing was changed.
    Unreadable(PathBuf),
    /// Entries ReviewQ no longer wants are still in these `hooks.json` files, usually because
    /// the user changed them; ReviewQ keeps owning them, `reason` is the last refusal, and every
    /// other entry was reconciled.
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
/// lock. When that file cannot be read, `hooks.json` is left as it is. An owned entry that
/// cannot be removed, usually because the user changed it, is kept and reported as `Kept`
/// after every other entry was tried. Fails when the ownership file cannot be written or an
/// entry cannot be installed; installation stops at the first entry it cannot write.
pub(super) fn reconcile(paths: &Paths, has_codex: bool) -> Result<Reconciled, Failure> {
    let sidecar = paths.state.join(OWNERSHIP);
    let Ok(owned) = paths::read_json::<Vec<Registration>>(&sidecar) else {
        return Ok(Reconciled::Unreadable(sidecar));
    };
    let mut owned = owned.unwrap_or_default();
    let wanted = if has_codex {
        Some((paths.codex_home.join("hooks.json"), command(paths)?))
    } else {
        None
    };
    let is_wanted = |hook: &Registration| {
        wanted
            .as_ref()
            .is_some_and(|(path, command)| hook.path == *path && hook.command == *command)
    };
    let mut kept = Vec::new();
    let mut retire_error = None;
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
            let index = if let Some(index) = location {
                Some(index)
            } else if let Some(hook) = Registration::plan(path, event, command)? {
                owned.push(hook);
                paths::write_json(&sidecar, &owned)?;
                Some(owned.len() - 1)
            } else {
                None
            };
            if let Some(index) = index {
                let hook = &mut owned[index];
                hook.install()?;
                if !hook.applied {
                    hook.applied = true;
                    paths::write_json(&sidecar, &owned)?;
                }
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
fn command(paths: &Paths) -> Result<String, Failure> {
    Ok(format!(
        "{} --profile {} internal codex-hook || :",
        shell_path(&paths.launcher)?,
        shell_path(&paths.profile)?,
    ))
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
        let Some(lock) = Lock::acquire(&self.path, Profile::Codex)? else {
            if installing {
                return Err(paths::conflict(&self.path));
            }
            return Ok(());
        };
        let before = read(&self.path)?;
        let bytes = before.as_ref().map(|file| file.bytes.as_slice());
        let change = if installing {
            json::install(bytes, self.event, &self.command, self.added, self.applied)?
        } else {
            json::remove(bytes, self.event, &self.command, self.added)?
        };
        let Some(change) = change else {
            return Ok(());
        };
        let after = change.map(|bytes| Existing {
            bytes,
            mode: before.as_ref().map_or(0o600, |file| file.mode),
        });
        lock.write(before.as_ref(), after.as_ref())?;
        lock.readback(after.as_ref())?;
        profile_config::sync_parent(&self.path)
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

        // An ownership file that cannot be read leaves hooks.json as it is.
        let current = fs::read(&file)?;
        fs::write(&sidecar, br#"[{"path":"hooks.json"}]"#)?;
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600))?;
        assert!(matches!(
            reconcile(&upgraded, true)?,
            Reconciled::Unreadable(path) if path == sidecar
        ));
        assert_eq!(fs::read(&file)?, current);
        Ok(())
    }
}
