//! Optional Codex report hook installed alongside the stable user launcher.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::checkout_setup::files::{Existing, read};
use crate::profile_config::{self, Lock, Profile};

use super::{Failure, Paths, paths};

mod json;

const MATCHER: &str = "^mcp__reviewq__report_handoff$";
const TIMEOUT_SECONDS: u32 = 5;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Registration {
    pub(super) path: PathBuf,
    command: String,
    added: json::Added,
    pub(super) applied: bool,
}

impl Registration {
    pub(super) fn plan(paths: &Paths, path: &Path) -> Result<Option<Self>, Failure> {
        let command = format!(
            "{} --profile {} internal codex-report-witness --codex-home {}",
            shell_path(&paths.launcher)?,
            shell_path(&paths.profile)?,
            shell_path(&paths.codex_home)?,
        );
        let file = read(path)?;
        let plan = json::plan(file.as_ref().map(|file| file.bytes.as_slice()), &command)?;
        Ok(plan.map(|added| Self {
            path: path.to_owned(),
            command,
            added,
            applied: false,
        }))
    }

    pub(super) fn install(&self) -> Result<(), Failure> {
        self.edit(true)
    }

    pub(super) fn remove(&self) -> Result<(), Failure> {
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
    use std::fs;
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

        let mut fresh = Registration::plan(&paths, &file)?.expect("hook to be installed");
        fresh.install()?;
        let created = fs::read(&file)?;
        assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o777, 0o600);
        assert!(Registration::plan(&paths, &file)?.is_none());
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
        let mut installed = Registration::plan(&paths, &file)?.expect("owned hook to append");
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
        let installed = Registration::plan(&paths, &file)?.expect("hook to append");
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
        Ok(())
    }
}
