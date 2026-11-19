use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rustix::fs::OFlags;
use sha2::{Digest, Sha256};

use super::Failure;

const FILE_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct Existing {
    pub bytes: Vec<u8>,
    pub mode: u32,
}

pub(super) struct Edit {
    path: PathBuf,
    before: Option<Existing>,
    after: Option<Vec<u8>>,
    mode: u32,
}

pub(super) struct Changes {
    edits: Vec<Edit>,
    committed: usize,
    pub rollback_conflict: bool,
}

fn credential_path(profile: &Path, common: &Path) -> Result<PathBuf, Failure> {
    let directory = profile.join("checkout-grants");
    create_dir(&directory)?;
    if directory.metadata()?.mode() & 0o777 != 0o700 {
        return Err("Checkout credential storage must be owner-only.".into());
    }
    let hash = URL_SAFE_NO_PAD.encode(Sha256::digest(common.as_os_str().as_bytes()));
    Ok(directory.join(hash))
}

pub(super) fn hold(profile: &Path, common: &Path) -> Result<File, Failure> {
    let flags = (OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32;
    let path = credential_path(profile, common)?.with_extension("lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(flags)
        .open(&path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.nlink() != 1
        || meta.mode() & 0o777 != 0o600
    {
        return Err("Checkout lock must be owner-only.".into());
    }
    file.try_lock()
        .map_err(|_| "Another command is editing this checkout.")?;
    Ok(file)
}

pub(super) fn hold_client(profile: &Path, path: &Path) -> Result<File, Failure> {
    hold(profile, path)
}

pub(super) fn inventory(profile: &Path, common: &Path) -> Result<PathBuf, Failure> {
    Ok(credential_path(profile, common)?.with_extension("json"))
}

pub(super) fn hook_inventory(profile: &Path, common: &Path) -> Result<PathBuf, Failure> {
    Ok(credential_path(profile, common)?.with_extension("hooks.json"))
}

pub(super) fn credential(profile: &Path, common: &Path) -> Result<[u8; 32], Failure> {
    if let Some(key) = existing_credential(profile, common)? {
        return Ok(key);
    }
    let path = credential_path(profile, common)?;
    let mut key = [0; 32];
    getrandom::fill(&mut key).map_err(|_| "Could not generate checkout credentials.")?;
    let mut stage =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing credential directory.")?)?;
    stage
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    stage.write_all(&key)?;
    stage.as_file().sync_all()?;
    stage
        .persist_noclobber(path)
        .map_err(|_| "Could not publish the private checkout credential.")?;
    Ok(key)
}

pub(super) fn existing_credential(
    profile: &Path,
    common: &Path,
) -> Result<Option<[u8; 32]>, Failure> {
    read(&credential_path(profile, common)?)?
        .map(|saved| {
            if saved.mode != 0o600 {
                return Err("Checkout credential mode is not owner-only.".into());
            }
            saved
                .bytes
                .try_into()
                .map_err(|_| "Invalid checkout credential.".into())
        })
        .transpose()
}

pub(super) fn forget(profile: &Path, common: &Path) -> Result<(), Failure> {
    let path = credential_path(profile, common)?;
    if let Some(saved) = read(&path)? {
        if saved.mode != 0o600 || saved.bytes.len() != 32 {
            return Err("Invalid checkout credential was preserved.".into());
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(super) fn read(path: &Path) -> Result<Option<Existing>, Failure> {
    read_limited(path, FILE_LIMIT)
}

pub(super) fn read_limited(path: &Path, limit: u64) -> Result<Option<Existing>, Failure> {
    let flags = (OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32;
    let file = match OpenOptions::new().read(true).custom_flags(flags).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.nlink() != 1
        || path.canonicalize()? != path
    {
        return Err("Expected an owned regular checkout file.".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("Checkout file exceeds the operation limit.".into());
    }
    Ok(Some(Existing {
        bytes,
        mode: metadata.mode() & 0o777,
    }))
}

impl Edit {
    pub(super) fn new(
        path: PathBuf,
        before: Option<Existing>,
        after: Vec<u8>,
        private: bool,
    ) -> Self {
        let mode = if private {
            0o600
        } else {
            before.as_ref().map_or(0o644, |before| before.mode)
        };
        Self {
            path,
            before,
            after: Some(after),
            mode,
        }
    }

    pub(super) fn remove(path: PathBuf, before: Existing) -> Self {
        Self {
            path,
            before: Some(before),
            after: None,
            mode: 0,
        }
    }

    pub(super) fn executable(path: PathBuf, before: Option<Existing>, bytes: Vec<u8>) -> Self {
        let mut edit = Self::new(path, before, bytes, true);
        edit.mode = 0o700;
        edit
    }

    pub(super) fn apply(&self) -> Result<(), Failure> {
        if self.unchanged() {
            return Ok(());
        }
        let expected = self.before.as_ref().map(|item| item.bytes.as_slice());
        self.replace(expected)?;
        self.sync()
    }

    fn replace(&self, expected: Option<&[u8]>) -> Result<(), Failure> {
        match &self.after {
            Some(bytes) => write(&self.path, bytes, self.mode, expected),
            None => {
                same(&self.path, expected)?;
                fs::remove_file(&self.path).map_err(Into::into)
            }
        }
    }

    fn sync(&self) -> Result<(), Failure> {
        let dir = self
            .path
            .parent()
            .ok_or("Missing checkout destination directory.")?;
        Ok(File::open(dir)?.sync_all()?)
    }

    fn unchanged(&self) -> bool {
        self.before.as_ref().is_some_and(|before| {
            Some((before.mode, before.bytes.as_slice()))
                == self.after.as_deref().map(|after| (self.mode, after))
        })
    }

    fn rollback(&self) -> Result<(), Failure> {
        same(&self.path, self.after.as_deref())?;
        match &self.before {
            Some(before) => write(
                &self.path,
                &before.bytes,
                before.mode,
                self.after.as_deref(),
            ),
            None => fs::remove_file(&self.path).map_err(Into::into),
        }
    }
}

impl Changes {
    pub(super) fn new(mut edits: Vec<Edit>) -> Self {
        edits.retain(|edit| !edit.unchanged());
        Self {
            edits,
            committed: 0,
            rollback_conflict: false,
        }
    }

    pub(super) fn apply(&mut self) -> Result<(), Failure> {
        let forward = self.edits.iter().try_for_each(|edit| {
            edit.replace(edit.before.as_ref().map(|file| file.bytes.as_slice()))?;
            self.committed += 1;
            edit.sync()
        });
        if forward.is_err() {
            self.rollback()?;
        }
        forward
    }

    pub(super) fn rollback(&mut self) -> Result<(), Failure> {
        let mut failed = false;
        for edit in self.edits[..self.committed].iter().rev() {
            failed |= edit.rollback().is_err();
        }
        self.committed = 0;
        self.rollback_conflict |= failed;
        if failed {
            return Err("Checkout changed during setup. Divergent files were preserved.".into());
        }
        Ok(())
    }
}

fn same(path: &Path, expected: Option<&[u8]>) -> Result<(), Failure> {
    if read(path)?.as_ref().map(|item| item.bytes.as_slice()) != expected {
        return Err("Checkout file changed during setup.".into());
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8], mode: u32, expected: Option<&[u8]>) -> Result<(), Failure> {
    let directory = path
        .parent()
        .ok_or("Missing checkout destination directory.")?;
    create_dir(directory)?;
    let mut staged = tempfile::NamedTempFile::new_in(directory)?;
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    same(path, expected)?;
    if expected.is_none() {
        staged.persist_noclobber(path)
    } else {
        staged.persist(path)
    }
    .map_err(|_| "Could not publish checkout configuration.")?;
    Ok(())
}

fn create_dir(path: &Path) -> Result<(), Failure> {
    match path.symlink_metadata() {
        Ok(metadata) => {
            if !metadata.is_dir()
                || path.canonicalize()? != path
                || metadata.uid() != rustix::process::getuid().as_raw()
            {
                return Err("Expected an owned checkout directory.".into());
            }
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            create_dir(path.parent().ok_or("Missing destination parent.")?)?;
            DirBuilder::new().mode(0o700).create(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
