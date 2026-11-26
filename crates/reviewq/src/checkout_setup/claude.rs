use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::profile_config::{self, Profile, Replacement};

use super::files::{Existing, read};
use super::{Failure, git};

mod json;

pub(super) struct Edit {
    file: PathBuf,
    retired: Vec<PathBuf>,
    selected: Option<Selection>,
    urls: BTreeSet<String>,
    bearer: String,
    applied: Option<Applied>,
}

struct Selection {
    checkout: PathBuf,
    url: String,
    owned: bool,
}

struct Applied {
    before: Option<Existing>,
    after: Option<Existing>,
    undo: Vec<json::Undo>,
}

impl Edit {
    pub(super) async fn prepare(
        file: &Path,
        retired: &[&Path],
        selected: Option<(&Path, &str, bool)>,
        urls: &BTreeSet<String>,
        bearer: &str,
    ) -> Result<Self, Failure> {
        if !retired.is_empty() {
            git::private_config(file, false).await?;
        }
        let edit = Self {
            file: file.to_owned(),
            retired: retired
                .iter()
                .map(|checkout| checkout.to_path_buf())
                .collect(),
            selected: selected.map(|(checkout, url, owned)| Selection {
                checkout: checkout.to_owned(),
                url: url.to_owned(),
                owned,
            }),
            urls: urls.clone(),
            bearer: bearer.to_owned(),
            applied: None,
        };
        // Find incompatible entries before issuing a grant. The write itself
        // recalculates the patch from the file it replaces.
        edit.changes(read(file)?.as_ref())?;
        Ok(edit)
    }

    /// Writes the entries, computed again from the file if Claude changes it before the
    /// write; `profile_config::replace` owns that loop and Claude's lock.
    pub(super) fn apply(&mut self) -> Result<(), Failure> {
        let replaced = profile_config::replace(&self.file, Profile::Claude, |before| {
            let (after, undo) = self.changes(before)?;
            Ok(after.map(|after| (Some(after), undo)))
        })?;
        let (before, after, undo, verified) = match replaced {
            Replacement::Unchanged => return Ok(()),
            Replacement::Missing => {
                return Err("The selected Claude profile directory is missing.".into());
            }
            Replacement::Replaced {
                before,
                after,
                value,
                verified,
            } => (before, after, value, verified),
        };
        self.applied = Some(Applied {
            before,
            after,
            undo,
        });
        verified?;
        profile_config::sync_parent(&self.file)
    }

    /// Reverses the entries `apply` wrote, computed again from the file if Claude changes it
    /// before the write, as there.
    pub(super) fn rollback(&mut self) -> Result<(), Failure> {
        let Some(applied) = &self.applied else {
            return Ok(());
        };
        let replaced = profile_config::replace(&self.file, Profile::Claude, |current| {
            if profile_config::same(current, applied.before.as_ref()) {
                return Ok(None);
            }
            let current = current.ok_or("The Claude profile config changed during setup.")?;
            if profile_config::same(Some(current), applied.after.as_ref()) {
                return Ok(Some((applied.before.clone(), ())));
            }
            // A concurrent writer may have changed another project, or Claude
            // may have reformatted the whole document. Reverse our entries only.
            let mut bytes = None;
            for undo in applied.undo.iter().rev() {
                if let Some(restored) = undo.restore(bytes.as_deref().unwrap_or(&current.bytes))? {
                    bytes = Some(restored);
                }
            }
            Ok(bytes.map(|bytes| (Some(Existing { bytes, mode: 0o600 }), ())))
        })?;
        self.applied = None;
        match replaced {
            Replacement::Unchanged => Ok(()),
            Replacement::Missing => {
                Err("The Claude profile directory changed during setup.".into())
            }
            Replacement::Replaced { verified, .. } => {
                verified?;
                profile_config::sync_parent(&self.file)
            }
        }
    }

    fn changes(
        &self,
        before: Option<&Existing>,
    ) -> Result<(Option<Existing>, Vec<json::Undo>), Failure> {
        let original = before.map(|file| file.bytes.as_slice());
        let mut after = None;
        let mut undo = Vec::new();
        if original.is_some() {
            for checkout in &self.retired {
                if let Some(patch) = json::edit(
                    after.as_deref().or(original),
                    checkout,
                    &self.urls,
                    &self.bearer,
                    None,
                )? {
                    after = Some(patch.bytes);
                    undo.push(patch.undo);
                }
            }
        }
        if let Some(selected) = &self.selected {
            let empty = BTreeSet::new();
            let urls = if selected.owned { &self.urls } else { &empty };
            if let Some(patch) = json::edit(
                after.as_deref().or(original),
                &selected.checkout,
                urls,
                &self.bearer,
                Some(&selected.url),
            )? {
                after = Some(patch.bytes);
                undo.push(patch.undo);
            } else if after.is_none() && before.is_some_and(|file| file.mode != 0o600) {
                after = original.map(<[u8]>::to_vec);
            }
        }
        Ok((after.map(|bytes| Existing { bytes, mode: 0o600 }), undo))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::*;

    #[tokio::test]
    async fn rollback_preserves_other_checkouts_and_native_changes() -> Result<(), Failure> {
        let profile = tempfile::tempdir()?;
        let root = profile.path().canonicalize()?;
        let file = root.join(".claude.json");
        let native_lock = root.join(".claude.json.lock");
        let original = b"{\"metrics\" : [0.1234567890123456789, 9e-5]}\n";
        fs::write(&file, original)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        let first_url = "http://127.0.0.1:8765/mcp";
        let second_url = "http://127.0.0.1:8766/mcp";
        let mut first = Edit::prepare(
            &file,
            &[],
            Some((Path::new("/repo/first"), first_url, false)),
            &BTreeSet::new(),
            "first-test",
        )
        .await?;
        let mut second = Edit::prepare(
            &file,
            &[],
            Some((Path::new("/repo/second"), second_url, false)),
            &BTreeSet::new(),
            "second-test",
        )
        .await?;

        fs::create_dir(&native_lock)?;
        assert!(first.apply().unwrap_err().to_string().contains("locked"));
        assert_eq!(fs::read(&file)?, original);
        assert!(native_lock.is_dir());
        fs::remove_dir(&native_lock)?;

        first.apply()?;
        second.apply()?;
        let native_update =
            fs::read_to_string(&file)?.replacen('{', "{\"nativePreference\" : true,", 1);
        fs::write(&file, &native_update)?;
        let inode = file.metadata()?.ino();
        Edit::prepare(
            &file,
            &[],
            Some((Path::new("/repo/second"), second_url, true)),
            &BTreeSet::from([second_url.to_owned()]),
            "second-test",
        )
        .await?
        .apply()?;
        assert_eq!(file.metadata()?.ino(), inode);

        first.rollback()?;
        let expected = native_update.replacen(
            r#""reviewq":{"type":"http","url":"http://127.0.0.1:8765/mcp","headers":{"Authorization":"Bearer first-test"}}"#,
            "",
            1,
        );
        assert_ne!(expected, native_update);
        assert_eq!(fs::read_to_string(&file)?, expected);
        assert!(expected.contains(second_url));

        let conflicting = expected.replace(second_url, "https://example.invalid/mcp");
        fs::write(&file, &conflicting)?;
        assert!(second.rollback().is_err());
        assert_eq!(fs::read_to_string(&file)?, conflicting);
        assert!(!native_lock.try_exists()?);
        Ok(())
    }
}
