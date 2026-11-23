use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::profile_config::{self, Lock, Profile};

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
    after: Existing,
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
        // recalculates the patch from the current file under Claude's lock.
        edit.changes(read(file)?.as_ref())?;
        Ok(edit)
    }

    pub(super) fn apply(&mut self) -> Result<(), Failure> {
        let Some(lock) = Lock::acquire(&self.file, Profile::Claude)? else {
            if self.selected.is_none() {
                return Ok(());
            }
            return Err("The selected Claude profile directory is missing.".into());
        };
        let before = read(&self.file)?;
        let (after, undo) = self.changes(before.as_ref())?;
        let Some(after) = after else {
            return Ok(());
        };
        lock.write(before.as_ref(), Some(&after))?;
        let verified = lock.readback(Some(&after));
        self.applied = Some(Applied {
            before,
            after,
            undo,
        });
        drop(lock);
        verified?;
        profile_config::sync_parent(&self.file)
    }

    pub(super) fn rollback(&mut self) -> Result<(), Failure> {
        let Some(applied) = &self.applied else {
            return Ok(());
        };
        let lock = Lock::acquire(&self.file, Profile::Claude)?
            .ok_or("The Claude profile directory changed during setup.")?;
        let current = read(&self.file)?;
        if profile_config::same(current.as_ref(), applied.before.as_ref()) {
            self.applied = None;
            return Ok(());
        }
        let current = current.ok_or("The Claude profile config changed during setup.")?;
        let verified = if profile_config::same(Some(&current), Some(&applied.after)) {
            lock.write(Some(&current), applied.before.as_ref())?;
            lock.readback(applied.before.as_ref())
        } else {
            // A concurrent writer may have changed another project, or Claude
            // may have reformatted the whole document. Reverse our entries only.
            let mut bytes = None;
            for undo in applied.undo.iter().rev() {
                if let Some(restored) = undo.restore(bytes.as_deref().unwrap_or(&current.bytes))? {
                    bytes = Some(restored);
                }
            }
            match bytes {
                Some(bytes) => {
                    let restored = Existing { bytes, mode: 0o600 };
                    lock.write(Some(&current), Some(&restored))?;
                    lock.readback(Some(&restored))
                }
                None => Ok(()),
            }
        };
        self.applied = None;
        drop(lock);
        verified?;
        profile_config::sync_parent(&self.file)
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
