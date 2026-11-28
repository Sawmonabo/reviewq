use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions, TryLockError};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use serde::{Serialize, de::DeserializeOwned};

use super::{Failure, bundle};

#[derive(Clone)]
pub(crate) struct Paths {
    pub home: PathBuf,
    pub profile: PathBuf,
    pub config_home: PathBuf,
    pub state_home: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub launcher: PathBuf,
    pub unit: PathBuf,
    pub start_socket: PathBuf,
    /// `reviewq-start@.service`, the template each start door connection runs.
    pub start_door: PathBuf,
    pub codex_home: PathBuf,
    pub claude_home: PathBuf,
    pub claude_home_is_explicit: bool,
}

impl Paths {
    pub(crate) fn resolve() -> Result<Self, Failure> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set.")?;
        absolute(&home)?;
        let xdg = |name: &str, fallback: &str| {
            env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
        };
        let profile = crate::runtime::profile(None)?;
        let config_home = xdg("XDG_CONFIG_HOME", ".config");
        let state_home = xdg("XDG_STATE_HOME", ".local/state");
        let codex_home = xdg("CODEX_HOME", ".codex");
        let claude_config_dir = env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
        let claude_home_is_explicit = claude_config_dir.is_some();
        let claude_home = claude_config_dir.unwrap_or_else(|| home.join(".claude"));
        for path in [
            &profile,
            &config_home,
            &state_home,
            &codex_home,
            &claude_home,
        ] {
            absolute(path)?;
        }
        Ok(Self {
            profile,
            launcher: home.join(".local/bin/reviewq"),
            config: config_home.join("reviewq"),
            unit: config_home.join("systemd/user/reviewq.service"),
            start_socket: config_home.join("systemd/user/reviewq-start.socket"),
            start_door: config_home.join("systemd/user/reviewq-start@.service"),
            state: state_home.join("reviewq"),
            home,
            config_home,
            state_home,
            codex_home,
            claude_home,
            claude_home_is_explicit,
        })
    }

    pub(super) fn prepare(&self) -> Result<(), Failure> {
        for path in [&self.profile, &self.state, &self.config] {
            ensure_dir(path, true)?;
        }
        ensure_dir(&self.profile.join("releases"), true)?;
        ensure_dir(parent(&self.launcher)?, false)?;
        ensure_dir(parent(&self.unit)?, false)
    }

    /// The installation lock's file. It outlives every installation; only an uninstall that
    /// deletes the data deletes it, last of all and while holding it.
    pub(super) fn lock_path(&self) -> PathBuf {
        self.state.join("install.lock")
    }

    /// Holds the installation lock, waiting while an install, uninstall, start or stop has it.
    pub(crate) fn lock(&self) -> Result<File, Failure> {
        loop {
            let file = self.lock_file()?;
            file.lock()?;
            if let Some(file) = self.still_named(file)? {
                return Ok(file);
            }
        }
    }

    /// Holds the installation lock without waiting, or returns `None` while another
    /// install, uninstall, start or stop has it.
    pub(crate) fn try_lock(&self) -> Result<Option<File>, Failure> {
        loop {
            let file = self.lock_file()?;
            match file.try_lock() {
                Ok(()) => {
                    if let Some(file) = self.still_named(file)? {
                        return Ok(Some(file));
                    }
                }
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }

    fn lock_file(&self) -> Result<File, Failure> {
        loop {
            ensure_dir(&self.state, true)?;
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(self.lock_path())
            {
                // The uninstall that deleted the lock's file removed its folder in between.
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                opened => return Ok(opened?),
            }
        }
    }

    /// The locked `file` when it is still the lock file at its path, or `None` when an
    /// uninstall deleted that file while this process waited for it: the caller then locks
    /// the file there now. Checked before `private`, which refuses a deleted file.
    fn still_named(&self, file: File) -> Result<Option<File>, Failure> {
        let path = self.lock_path();
        let held = file.metadata()?;
        match path.symlink_metadata() {
            Ok(named) if named.dev() == held.dev() && named.ino() == held.ino() => {
                private(&file, &path)?;
                Ok(Some(file))
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Returns once no install, uninstall or start holds the installation lock. Creates
    /// nothing; a missing or unreadable lock file counts as free.
    pub(crate) async fn wait_unlocked(&self) {
        let path = self.lock_path();
        loop {
            let held = OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(&path)
                .is_ok_and(|file| matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock)));
            if !held {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

pub(crate) fn ensure_dir(path: &Path, owner_only: bool) -> Result<(), Failure> {
    absolute(path)?;
    let user = rustix::process::getuid().as_raw();
    let mut current = PathBuf::from("/");
    for name in path.components().filter_map(|part| match part {
        Component::Normal(name) => Some(name),
        _ => None,
    }) {
        current.push(name);
        match current.symlink_metadata() {
            Ok(meta) if meta.is_dir() => (),
            Ok(_) => return Err(conflict(&current)),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if parent(&current)?.metadata()?.uid() != user {
                    return Err(conflict(&current));
                }
                match DirBuilder::new().mode(0o700).create(&current) {
                    Ok(()) => sync(parent(&current)?)?,
                    Err(error)
                        if error.kind() == ErrorKind::AlreadyExists
                            && current.symlink_metadata().is_ok_and(|meta| meta.is_dir()) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    let info = path.symlink_metadata()?;
    if !info.is_dir() || info.uid() != user || (owner_only && info.mode() & 0o777 != 0o700) {
        return Err(conflict(path));
    }
    Ok(())
}

fn absolute(path: &Path) -> Result<(), Failure> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(format!("Expected a canonical absolute path at {}", path.display()).into());
    }
    Ok(())
}

pub(super) fn parent(path: &Path) -> Result<&Path, Failure> {
    path.parent().ok_or_else(|| conflict(path))
}

pub(super) fn conflict(path: &Path) -> Failure {
    super::refused(format!(
        "Preserved a conflicting entry at {}",
        path.display()
    ))
}

pub(super) fn sync(path: &Path) -> Result<(), Failure> {
    File::open(path)?.sync_all()?;
    Ok(())
}

pub(super) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Failure> {
    read_json_with_limit(path, 4 * 1024 * 1024)
}

pub(super) fn read_json_with_limit<T: DeserializeOwned>(
    path: &Path,
    limit: u64,
) -> Result<Option<T>, Failure> {
    if !exists(path)? {
        return Ok(None);
    }
    let file = bundle::regular(path)?;
    private(&file, path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(conflict(path));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Failure> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    atomic_file(path, &bytes, exists(path)?)
}

pub(super) fn private(file: &File, path: &Path) -> Result<(), Failure> {
    let info = file.metadata()?;
    if !info.is_file()
        || info.nlink() != 1
        || info.mode() & 0o777 != 0o600
        || info.uid() != rustix::process::getuid().as_raw()
    {
        return Err(conflict(path));
    }
    Ok(())
}

pub(crate) fn exists(path: &Path) -> Result<bool, Failure> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn atomic_file(path: &Path, bytes: &[u8], replace: bool) -> Result<(), Failure> {
    let directory = parent(path)?;
    let mut stage = tempfile::Builder::new()
        .prefix(".reviewq-write.")
        .permissions(Permissions::from_mode(0o600))
        .tempfile_in(directory)?;
    stage.write_all(bytes)?;
    stage.as_file().sync_all()?;
    if replace {
        stage.persist(path)?;
    } else {
        stage.persist_noclobber(path)?;
    }
    sync(directory)
}

pub(super) fn link(path: &Path, target: &Path, replace: bool) -> Result<(), Failure> {
    let directory = parent(path)?;
    let stage = tempfile::Builder::new()
        .prefix(".reviewq-link.")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(directory)?;
    let sibling = stage.path().join("link");
    symlink(target, &sibling)?;
    if replace {
        fs::rename(&sibling, path)?;
    } else {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &sibling,
            rustix::fs::CWD,
            path,
            rustix::fs::RenameFlags::NOREPLACE,
        )?;
    }
    sync(directory)
}

#[cfg(test)]
mod tests {
    use super::{Failure, Paths};

    #[test]
    fn a_waiter_relocks_once_an_uninstall_deletes_the_lock_file() -> Result<(), Failure> {
        let home = tempfile::tempdir()?;
        let home = home.path().canonicalize()?;
        let paths = Paths {
            profile: home.join("profile"),
            config_home: home.join("config"),
            state_home: home.join("state"),
            config: home.join("config/reviewq"),
            state: home.join("state/reviewq"),
            launcher: home.join("bin/reviewq"),
            unit: home.join("config/systemd/user/reviewq.service"),
            start_socket: home.join("config/systemd/user/reviewq-start.socket"),
            start_door: home.join("config/systemd/user/reviewq-start@.service"),
            codex_home: home.join("codex"),
            claude_home: home.join("claude"),
            claude_home_is_explicit: false,
            home,
        };
        let uninstall = paths.lock()?;
        // A waiter opened the file before the uninstall deleted it, and locks it once released.
        let waiter = paths.lock_file()?;
        std::fs::remove_file(paths.lock_path())?;
        drop(uninstall);
        waiter.try_lock()?;
        assert!(paths.still_named(waiter)?.is_none());
        // It locks the new file instead, so the next process waits rather than sharing it.
        let _held = paths.lock()?;
        assert!(paths.try_lock()?.is_none());
        Ok(())
    }
}
