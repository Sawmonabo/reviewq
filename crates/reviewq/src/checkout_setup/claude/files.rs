use std::fs::{self, DirBuilder, File, Metadata, OpenOptions, Permissions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{CWD, Mode, OFlags, RenameFlags, renameat_with};

use super::{Existing, Failure, read};

// Claude Code 2.1.330 considers the empty lock directory stale after ten seconds.
const STALE_AFTER: Duration = Duration::from_secs(10);
const LOCK_LOST: &str = "ReviewQ lost the Claude profile lock. Retry checkout setup.";
const CONFIG_CHANGED: &str =
    "Claude changed its profile config during setup. Retry checkout setup.";

pub(super) struct Lock<'a> {
    config: &'a Path,
    path: PathBuf,
    directory: File,
    created: Instant,
}

impl<'a> Lock<'a> {
    pub(super) fn acquire(config: &'a Path) -> Result<Option<Self>, Failure> {
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
            return Err("Expected an owned Claude profile directory.".into());
        }
        let mut filename = config.as_os_str().to_owned();
        filename.push(".lock");
        let path = PathBuf::from(filename);
        let created = Instant::now();
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => (),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return Err(format!(
                    "The Claude profile is locked at {}. Retry when Claude finishes.",
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
            path,
            directory,
            created,
        };
        lock.check()?;
        Ok(Some(lock))
    }

    pub(super) fn write(
        &self,
        before: Option<&Existing>,
        after: Option<&Existing>,
    ) -> Result<(), Failure> {
        let stage = tempfile::Builder::new()
            .prefix(".reviewq-claude-")
            .permissions(Permissions::from_mode(0o700))
            .tempdir_in(parent(self.config)?)?;
        let candidate = stage.path().join("config");
        if let Some(after) = after {
            stage_file(&candidate, &after.bytes, after.mode)?;
        }
        self.readback(before)?;
        self.publish(stage, before, after)
    }

    pub(super) fn readback(&self, expected: Option<&Existing>) -> Result<(), Failure> {
        if !same(read(self.config)?.as_ref(), expected) {
            return Err(CONFIG_CHANGED.into());
        }
        self.check()
    }

    fn publish(
        &self,
        mut stage: tempfile::TempDir,
        before: Option<&Existing>,
        after: Option<&Existing>,
    ) -> Result<(), Failure> {
        let candidate = stage.path().join("config");
        let Some(before) = before else {
            self.check()?;
            return Ok(rename(&candidate, self.config, RenameFlags::NOREPLACE)?);
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
            return Ok(());
        }
        let restored = displaced.ok().flatten().is_some_and(|displaced| {
            self.restore_displaced(&candidate, &displaced, after.zip(staged_identity.as_ref()))
                .is_ok()
        });
        if restored {
            stage.disable_cleanup(false);
            return Err(CONFIG_CHANGED.into());
        }
        Err(format!(
            "Concurrent Claude settings writes prevented recovery. Displaced data was saved under {}.",
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
                return Err(CONFIG_CHANGED.into());
            }
            self.check()?;
            rename(candidate, self.config, RenameFlags::EXCHANGE)?;
            if !is_candidate(candidate, identity, replacement) {
                return Err(CONFIG_CHANGED.into());
            }
        } else {
            self.check()?;
            rename(candidate, self.config, RenameFlags::NOREPLACE)?;
        }
        if !is_candidate(self.config, &original, displaced) {
            return Err(CONFIG_CHANGED.into());
        }
        Ok(())
    }

    fn check(&self) -> Result<(), Failure> {
        if !self.still_owned(STALE_AFTER / 2) {
            return Err(LOCK_LOST.into());
        }
        Ok(())
    }

    fn still_owned(&self, max_age: Duration) -> bool {
        self.directory.metadata().is_ok_and(|directory| {
            self.path.symlink_metadata().is_ok_and(|current| {
                identity(&directory) == identity(&current)
                    && current.modified().is_ok_and(|modified| {
                        modified.elapsed().unwrap_or_default() < max_age
                            && self.created.elapsed() < max_age
                    })
            })
        })
    }
}

impl Drop for Lock<'_> {
    fn drop(&mut self) {
        if self.still_owned(STALE_AFTER) {
            let _ = fs::remove_dir(&self.path);
        }
    }
}

pub(super) fn same(left: Option<&Existing>, right: Option<&Existing>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.mode == right.mode && left.bytes == right.bytes,
        (None, None) => true,
        _ => false,
    }
}

pub(super) fn sync_parent(config: &Path) -> Result<(), Failure> {
    Ok(File::open(parent(config)?)?.sync_all()?)
}

fn parent(path: &Path) -> Result<&Path, Failure> {
    path.parent()
        .ok_or("Missing Claude profile directory.".into())
}

fn rename(from: &Path, to: &Path, flags: RenameFlags) -> Result<(), rustix::io::Errno> {
    renameat_with(CWD, from, CWD, to, flags)
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
        let lock = Lock::acquire(&config)?.expect("profile directory exists");
        let stage = tempfile::tempdir_in(&directory)?;
        stage_file(&stage.path().join("config"), &after.bytes, after.mode)?;
        lock.readback(Some(&before))?;

        let native = directory.join("native-stage");
        let native_bytes = br#"{"number" : 0.1234567890123456789}"#;
        stage_file(&native, native_bytes, 0o600)?;
        fs::rename(&native, &config)?;
        let native_inode = config.metadata()?.ino();
        assert_eq!(
            lock.publish(stage, Some(&before), Some(&after))
                .unwrap_err()
                .to_string(),
            CONFIG_CHANGED,
        );
        assert_eq!(fs::read(&config)?, native_bytes);
        assert_eq!(config.metadata()?.ino(), native_inode);
        assert_eq!(directory.read_dir()?.count(), 2);
        Ok(())
    }
}
