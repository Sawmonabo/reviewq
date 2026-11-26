//! Compare and atomically replace native profile files without losing concurrent edits.

use std::fs::{self, DirBuilder, File, Metadata, OpenOptions, Permissions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{CWD, Mode, OFlags, RenameFlags, renameat_with};

use crate::checkout_setup::files::{Existing, read};

type Failure = Box<dyn std::error::Error + Send + Sync>;

// Claude Code 2.1.330 considers the empty lock directory stale after ten seconds.
const STALE_AFTER: Duration = Duration::from_secs(10);
#[derive(Clone, Copy)]
pub(crate) enum Profile {
    Claude,
    Codex,
}

impl Profile {
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }
}

struct Lock<'a> {
    config: &'a Path,
    native: Option<NativeLock>,
    profile: Profile,
}

struct NativeLock {
    path: PathBuf,
    directory: File,
    created: Instant,
}

/// How many times `replace` reads, computes and stages again after another writer changed the
/// config before ReviewQ took the lock or during its rename.
const ATTEMPTS: usize = 5;

/// What `replace` did with a profile config.
pub(crate) enum Replacement<T> {
    /// The profile directory is missing.
    Missing,
    /// The computation asked for no change.
    Unchanged,
    /// The config was replaced: what it read as, what it now holds (`None` when it was
    /// removed), the computation's own value, and whether the config still read as `after`
    /// under the lock. A failed check leaves the replacement in place.
    Replaced {
        before: Option<Existing>,
        after: Option<Existing>,
        value: T,
        verified: Result<(), Failure>,
    },
}

/// Replaces `config` with what `compute` makes of its current contents: `None` for no change,
/// or the new contents (`None` to remove the file) and a value to return beside them. The
/// replacement is written and synced in a private directory beside the config before the lock
/// is taken, because a sync can take seconds on a busy disk, and the lock is held only to
/// confirm that the config still reads as it did when `compute` saw it and to rename. When
/// another writer changed it in between, or during the rename, its change is kept and the
/// config is read, computed and staged again, up to `ATTEMPTS` times.
pub(crate) fn replace<T>(
    config: &Path,
    profile: Profile,
    mut compute: impl FnMut(Option<&Existing>) -> Result<Option<(Option<Existing>, T)>, Failure>,
) -> Result<Replacement<T>, Failure> {
    for _ in 0..ATTEMPTS {
        let before = read(config)?;
        let Some((after, value)) = compute(before.as_ref())? else {
            return Ok(Replacement::Unchanged);
        };
        let Some(parent) = profile_directory(config, profile)? else {
            return Ok(Replacement::Missing);
        };
        let stage = staging_directory(parent)?;
        if let Some(after) = &after {
            stage_file(&stage.path().join("config"), &after.bytes, after.mode)?;
        }
        let Some(lock) = Lock::acquire(config, profile)? else {
            return Ok(Replacement::Missing);
        };
        if !lock.write(before.as_ref(), stage, after.as_ref())? {
            continue;
        }
        let verified = lock.readback(after.as_ref());
        return Ok(Replacement::Replaced {
            before,
            after,
            value,
            verified,
        });
    }
    Err(changed(profile))
}

/// The directory that holds `config`, after checking that the current user owns it and that
/// it is reached without a symbolic link. `None` when it is missing.
fn profile_directory(config: &Path, profile: Profile) -> Result<Option<&Path>, Failure> {
    let parent = parent(config)?;
    let metadata = match parent.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || parent.canonicalize()? != parent
    {
        return Err(format!("Expected an owned {} profile directory.", profile.name()).into());
    }
    Ok(Some(parent))
}

impl<'a> Lock<'a> {
    fn acquire(config: &'a Path, profile: Profile) -> Result<Option<Self>, Failure> {
        if profile_directory(config, profile)?.is_none() {
            return Ok(None);
        }
        // ReviewQ's install lock already serializes Codex lifecycle edits. The
        // exchange below also protects against concurrent writes by an editor.
        if matches!(profile, Profile::Codex) {
            return Ok(Some(Self {
                config,
                native: None,
                profile,
            }));
        }
        let mut filename = config.as_os_str().to_owned();
        filename.push(".lock");
        let path = PathBuf::from(filename);
        let created = Instant::now();
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => (),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return Err(format!(
                    "The {} profile is locked at {}.",
                    profile.name(),
                    path.display()
                )
                .into());
            }
            Err(error) => return Err(error.into()),
        }
        let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let directory = File::from(rustix::fs::open(&path, flags, Mode::empty())?);
        let lock = Self {
            config,
            native: Some(NativeLock {
                path,
                directory,
                created,
            }),
            profile,
        };
        lock.check()?;
        Ok(Some(lock))
    }

    /// Replaces the config with the file staged in `stage`, or removes it when `after` is
    /// `None`, when it still reads `before`. `false`, with the config as another writer left
    /// it, when it does not, or when a writer that ignores the lock changed it during the rename.
    fn write(
        &self,
        before: Option<&Existing>,
        stage: tempfile::TempDir,
        after: Option<&Existing>,
    ) -> Result<bool, Failure> {
        if !same(read(self.config)?.as_ref(), before) {
            return Ok(false);
        }
        self.check()?;
        self.publish(stage, before, after)
    }

    fn readback(&self, expected: Option<&Existing>) -> Result<(), Failure> {
        if !same(read(self.config)?.as_ref(), expected) {
            return Err(self.config_changed());
        }
        self.check()
    }

    fn publish(
        &self,
        mut stage: tempfile::TempDir,
        before: Option<&Existing>,
        after: Option<&Existing>,
    ) -> Result<bool, Failure> {
        let candidate = stage.path().join("config");
        let Some(before) = before else {
            self.check()?;
            return match rename(&candidate, self.config, RenameFlags::NOREPLACE) {
                Ok(()) => Ok(true),
                Err(rustix::io::Errno::EXIST) => Ok(false),
                Err(error) => Err(error.into()),
            };
        };
        let staged_identity = after.map(|_| candidate.symlink_metadata()).transpose()?;
        self.check()?;
        // Claude can use an unlocked fallback. Keep whatever the rename displaces
        // until its actual bytes and mode have been checked.
        stage.disable_cleanup(true);
        let committed = if after.is_some() {
            rename(&candidate, self.config, RenameFlags::EXCHANGE)
        } else {
            rename(self.config, &candidate, RenameFlags::NOREPLACE)
        };
        if let Err(error) = committed {
            stage.disable_cleanup(false);
            return Err(error.into());
        }
        let displaced = read(&candidate);
        if displaced
            .as_ref()
            .is_ok_and(|actual| same(actual.as_ref(), Some(before)))
        {
            stage.disable_cleanup(false);
            return Ok(true);
        }
        let restored = displaced.ok().flatten().is_some_and(|displaced| {
            self.restore_displaced(&candidate, &displaced, after.zip(staged_identity.as_ref()))
                .is_ok()
        });
        if restored {
            stage.disable_cleanup(false);
            return Ok(false);
        }
        Err(format!(
            "Concurrent {} settings writes prevented recovery. Displaced data was saved under {}.",
            self.profile.name(),
            stage.keep().display()
        )
        .into())
    }

    fn restore_displaced(
        &self,
        candidate: &Path,
        displaced: &Existing,
        replacement: Option<(&Existing, &Metadata)>,
    ) -> Result<(), Failure> {
        self.check()?;
        let original = candidate.symlink_metadata()?;
        // Keep the raced bytes even if another unlocked writer replaces the
        // restored file before it can be verified.
        stage_file(
            &candidate.with_file_name("displaced"),
            &displaced.bytes,
            0o600,
        )?;
        if let Some((replacement, identity)) = replacement {
            if !is_candidate(self.config, identity, replacement) {
                return Err(self.config_changed());
            }
            self.check()?;
            rename(candidate, self.config, RenameFlags::EXCHANGE)?;
            if !is_candidate(candidate, identity, replacement) {
                return Err(self.config_changed());
            }
        } else {
            self.check()?;
            rename(candidate, self.config, RenameFlags::NOREPLACE)?;
        }
        if !is_candidate(self.config, &original, displaced) {
            return Err(self.config_changed());
        }
        Ok(())
    }

    fn config_changed(&self) -> Failure {
        changed(self.profile)
    }

    fn check(&self) -> Result<(), Failure> {
        if !self.still_owned(STALE_AFTER / 2) {
            return Err(format!("ReviewQ lost the {} profile lock.", self.profile.name()).into());
        }
        Ok(())
    }

    fn still_owned(&self, max_age: Duration) -> bool {
        self.native.as_ref().is_none_or(|native| {
            native.directory.metadata().is_ok_and(|directory| {
                native.path.symlink_metadata().is_ok_and(|current| {
                    identity(&directory) == identity(&current)
                        && current.modified().is_ok_and(|modified| {
                            modified.elapsed().unwrap_or_default() < max_age
                                && native.created.elapsed() < max_age
                        })
                })
            })
        })
    }
}

impl Drop for Lock<'_> {
    fn drop(&mut self) {
        if let Some(native) = &self.native
            && self.still_owned(STALE_AFTER)
        {
            let _ = fs::remove_dir(&native.path);
        }
    }
}

fn changed(profile: Profile) -> Failure {
    format!(
        "{} changed its profile config during setup.",
        profile.name()
    )
    .into()
}

pub(crate) fn same(left: Option<&Existing>, right: Option<&Existing>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.mode == right.mode && left.bytes == right.bytes,
        (None, None) => true,
        _ => false,
    }
}

pub(crate) fn sync_parent(config: &Path) -> Result<(), Failure> {
    Ok(File::open(parent(config)?)?.sync_all()?)
}

fn parent(path: &Path) -> Result<&Path, Failure> {
    path.parent()
        .ok_or("Missing native profile directory.".into())
}

fn rename(from: &Path, to: &Path, flags: RenameFlags) -> Result<(), rustix::io::Errno> {
    renameat_with(CWD, from, CWD, to, flags)
}

fn staging_directory(parent: &Path) -> Result<tempfile::TempDir, Failure> {
    let stage = tempfile::Builder::new()
        .prefix(".reviewq-profile-")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(parent)?;
    // A custom Claude profile can live in a Git worktree.
    fs::write(stage.path().join(".gitignore"), b"*\n")?;
    Ok(stage)
}

fn stage_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), Failure> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.set_permissions(Permissions::from_mode(mode))?;
    Ok(file.sync_all()?)
}

fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn is_candidate(path: &Path, original: &Metadata, contents: &Existing) -> bool {
    path.symlink_metadata()
        .is_ok_and(|current| identity(original) == identity(&current))
        && read(path).is_ok_and(|current| same(current.as_ref(), Some(contents)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_write_after_comparison_is_restored() -> Result<(), Failure> {
        let profile = tempfile::tempdir()?;
        let directory = profile.path().canonicalize()?;
        let config = directory.join(".claude.json");
        let before = Existing {
            bytes: b"{}".to_vec(),
            mode: 0o600,
        };
        let after = Existing {
            bytes: br#"{"reviewq":"test"}"#.to_vec(),
            mode: 0o600,
        };
        stage_file(&config, &before.bytes, before.mode)?;
        let lock = Lock::acquire(&config, Profile::Claude)?.expect("profile directory exists");
        let stage = staging_directory(&directory)?;
        stage_file(&stage.path().join("config"), &after.bytes, after.mode)?;
        assert!(same(read(&config)?.as_ref(), Some(&before)));

        let native = directory.join("native-stage");
        let native_bytes = br#"{"number" : 0.1234567890123456789}"#;
        stage_file(&native, native_bytes, 0o600)?;
        fs::rename(&native, &config)?;
        let native_inode = config.metadata()?.ino();
        assert!(!lock.publish(stage, Some(&before), Some(&after))?);
        assert_eq!(fs::read(&config)?, native_bytes);
        assert_eq!(config.metadata()?.ino(), native_inode);
        assert_eq!(directory.read_dir()?.count(), 2);
        drop(lock);

        // A native write between the read and the lock is kept, and the edit is computed
        // again from it instead of failing.
        let mut seen = Vec::new();
        let replaced = replace(&config, Profile::Claude, |current| {
            let current = current.expect("config exists");
            seen.push(current.bytes.clone());
            if seen.len() == 1 {
                fs::write(&config, br#"{"native":1}"#)?;
            }
            let mut bytes = current.bytes.clone();
            bytes.extend_from_slice(b" ");
            Ok(Some((Some(Existing { bytes, mode: 0o600 }), ())))
        })?;
        assert!(matches!(
            replaced,
            Replacement::Replaced {
                verified: Ok(()),
                ..
            }
        ));
        assert_eq!(seen, [native_bytes.to_vec(), br#"{"native":1}"#.to_vec()]);
        assert_eq!(fs::read(&config)?, br#"{"native":1} "#);
        assert_eq!(directory.read_dir()?.count(), 1);
        Ok(())
    }
}
