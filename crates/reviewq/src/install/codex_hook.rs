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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    path: PathBuf,
    command: String,
    added: json::Added,
    applied: bool,
}

/// The release journal is read by older binaries during rollback. Track the
/// optional hook separately under the same installation lock. `command` is the
/// hook's shell command, built with `shell_path`.
pub(super) fn reconcile(paths: &Paths, has_codex: bool, command: &str) -> Result<(), Failure> {
    let sidecar = paths.state.join(OWNERSHIP);
    let mut owned = paths::read_json::<Vec<Registration>>(&sidecar)?.unwrap_or_default();
    let wanted = has_codex.then(|| paths.codex_home.join("hooks.json"));
    let mut retire_error = None;
    let original_count = owned.len();
    owned.retain(|hook| {
        if wanted.as_ref() == Some(&hook.path) {
            return true;
        }
        match hook.remove() {
            Ok(()) => false,
            Err(error) => {
                retire_error = Some(error);
                true
            }
        }
    });
    if owned.len() != original_count {
        if owned.is_empty() {
            fs::remove_file(&sidecar)?;
            paths::sync(&paths.state)?;
        } else {
            paths::write_json(&sidecar, &owned)?;
        }
    }
    if let Some(wanted) = wanted {
        let location = owned.iter().position(|hook| hook.path == wanted);
        let index = if let Some(index) = location {
            Some(index)
        } else if let Some(hook) = Registration::plan(&wanted, command)? {
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
    retire_error.map_or(Ok(()), Err)
}

impl Registration {
    fn plan(path: &Path, command: &str) -> Result<Option<Self>, Failure> {
        let file = read(path)?;
        let plan = json::plan(file.as_ref().map(|file| file.bytes.as_slice()), command)?;
        Ok(plan.map(|added| Self {
            path: path.to_owned(),
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
            json::install(bytes, &self.command, self.added, self.applied)?
        } else {
            json::remove(bytes, &self.command, self.added)?
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
        let command = format!("{} --version", shell_path(&paths.launcher)?);
        let config = codex_home.join("config.toml");
        let trust = b"[hooks.state.example]\nenabled = false\n";
        fs::write(&config, trust)?;

        let mut fresh = Registration::plan(&file, &command)?.expect("hook to be installed");
        fresh.install()?;
        let created = fs::read(&file)?;
        assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o777, 0o600);
        assert!(Registration::plan(&file, &command)?.is_none());
        fresh.applied = true;
        fresh.install()?;
        assert_eq!(fs::read(&file)?, created);
        fresh.remove()?;
        assert!(!file.try_exists()?);
        assert!(fresh.install().is_err());

        let earlier =
            r#"{"matcher": "^Bash$", "hooks":[{"type":"command","command":"/bin/true"}]}"#;
        let later = r#"{"matcher":"^Edit$","hooks":[{"type":"command","command":"/bin/true"}]}"#;
        let original = format!(
            "{{ \"metadata\": 0.123456789012345678901, \"hooks\" : {{ \"PreToolUse\" : [ {earlier} ] }} }}\n"
        );
        fs::write(&file, &original)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640))?;
        let mut installed = Registration::plan(&file, &command)?.expect("owned hook to append");
        installed.install()?;
        let with_hook = fs::read_to_string(&file)?;
        let doc: serde_json::Value = serde_json::from_str(&with_hook)?;
        assert_eq!(
            doc["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
            installed.command
        );
        assert!(with_hook.contains("0.123456789012345678901"));
        installed.applied = true;
        installed.install()?;
        assert_eq!(fs::read_to_string(&file)?, with_hook);
        let owned_group = serde_json::to_string(&doc["hooks"]["PreToolUse"][1])?;
        let modified = with_hook.replace("\"timeout\":5", "\"timeout\":7");
        fs::write(&file, &modified)?;
        assert!(installed.remove().is_err());
        assert_eq!(fs::read_to_string(&file)?, modified);
        fs::write(&file, &with_hook)?;
        installed.remove()?;
        assert_eq!(fs::read_to_string(&file)?, original);
        let installed = Registration::plan(&file, &command)?.expect("hook to append");
        installed.install()?;
        let changed_by_user = format!(
            "{{ \"metadata\": 1.123456789012345678901, \"hooks\" : {{ \"PreToolUse\" : [ {earlier},{owned_group},{later} ] }} }}\n"
        );
        fs::write(&file, &changed_by_user)?;
        installed.remove()?;
        assert_eq!(
            fs::read_to_string(&file)?,
            changed_by_user.replace(&owned_group, r#"{"hooks":[]}"#),
        );
        assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o777, 0o640);
        assert_eq!(fs::read(&config)?, trust);
        assert!(!file.with_extension("json.lock").try_exists()?);

        fs::create_dir(&paths.state)?;
        let release_journal = paths.state.join("install.json");
        let release_bytes = b"{\"active\":null,\"store_id\":null,\"pending\":null,\"known\":[],\"removing\":false}\n";
        fs::write(&release_journal, release_bytes)?;
        reconcile(&paths, true, &command)?;
        let sidecar = paths.state.join(OWNERSHIP);
        assert_eq!(fs::metadata(&sidecar)?.permissions().mode() & 0o777, 0o600);
        let registrations =
            paths::read_json::<Vec<Registration>>(&sidecar)?.expect("hook ownership");
        assert!(registrations[0].applied);

        reconcile(&paths, false, &command)?;
        assert!(!sidecar.try_exists()?);
        assert_eq!(fs::read(&release_journal)?, release_bytes);
        assert_eq!(fs::read(&config)?, trust);
        assert_eq!(
            fs::read_to_string(&file)?,
            changed_by_user.replace(&owned_group, r#"{"hooks":[]}"#),
        );
        Ok(())
    }
}
