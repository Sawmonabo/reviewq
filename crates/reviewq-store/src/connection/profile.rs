//! Owned local filesystem paths and an exclusive profile lock.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use reviewq_app::access::AppError;
use reviewq_domain::session::AbsolutePath;
use rustix::fs::{self, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use rustix::process::geteuid;

use crate::StoreError;

pub(super) struct Profile {
    directory: File,
    path: PathBuf,
    name: OsString,
}

impl Profile {
    pub(super) fn resolve(path: &AbsolutePath) -> Result<Self, StoreError> {
        let path = path.as_path();
        let parent = path.parent().ok_or_else(|| {
            StoreError::path(path, "expected an existing private profile directory")
        })?;
        let name = path
            .file_name()
            .ok_or_else(|| StoreError::path(path, "expected a database filename"))?;
        let directory = open_parent(parent)?;
        let meta = directory.metadata()?;
        if meta.uid() != geteuid().as_raw() || meta.mode() & 0o7777 != 0o700 {
            return Err(StoreError::path(
                parent,
                "profile directory must be owned by the current user with mode 0700",
            ));
        }
        require_local_filesystem(&directory, parent)?;
        Ok(Self {
            directory,
            path: path.into(),
            name: name.into(),
        })
    }

    pub(super) fn try_lock(&self) -> Result<(), StoreError> {
        match self.directory.try_lock() {
            Ok(()) => Ok(()),
            Err(TryLockError::WouldBlock) => Err(AppError::Busy.into()),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn parent(&self) -> &Path {
        self.path.parent().expect("validated profile parent")
    }

    pub(super) fn file(&self, suffix: &str) -> Result<Option<File>, StoreError> {
        let mut name = self.name.clone();
        name.push(suffix);
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let opened = match fs::openat(&self.directory, &name, flags, Mode::empty()) {
            Ok(descriptor) => File::from(descriptor),
            Err(Errno::NOENT) => return Ok(None),
            Err(Errno::LOOP) => {
                return Err(StoreError::path(
                    &self.parent().join(&name),
                    "symbolic links are unsupported",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let meta = opened.metadata()?;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != geteuid().as_raw()
            || meta.mode() & 0o7777 != 0o600
        {
            return Err(StoreError::path(
                &self.parent().join(&name),
                "expected a regular, singly linked file owned by the current user with mode 0600",
            ));
        }
        Ok(Some(opened))
    }

    pub(super) fn publish(&self, staged: &Path) -> Result<(), StoreError> {
        fs::renameat_with(
            fs::CWD,
            staged,
            &self.directory,
            &self.name,
            RenameFlags::NOREPLACE,
        )?;
        self.directory.sync_all()?;
        Ok(())
    }
}

fn open_parent(parent: &Path) -> Result<File, StoreError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(fs::open("/", flags, Mode::empty())?);
    let mut visited = PathBuf::from("/");
    for component in parent.components() {
        let name = match component {
            Component::RootDir => continue,
            Component::Normal(name) => name,
            _ => {
                return Err(StoreError::path(
                    parent,
                    "expected a canonical absolute path",
                ));
            }
        };
        visited.push(name);
        directory = open_child(&directory, name, &visited, flags)?;
        if !trusted_directory(&directory.metadata()?) {
            return Err(StoreError::path(
                &visited,
                "unsafe parent directory ownership or permissions",
            ));
        }
    }
    Ok(directory)
}

fn open_child(
    directory: &File,
    name: &OsStr,
    path: &Path,
    flags: OFlags,
) -> Result<File, StoreError> {
    match fs::openat(directory, name, flags, Mode::empty()) {
        Ok(descriptor) => Ok(File::from(descriptor)),
        Err(Errno::NOTDIR | Errno::LOOP) => Err(StoreError::path(
            path,
            "expected a real directory without symlinks",
        )),
        Err(error) => Err(error.into()),
    }
}

fn trusted_directory(meta: &Metadata) -> bool {
    let owner = meta.uid();
    let mode = meta.mode();
    (owner == 0 || owner == geteuid().as_raw())
        && (mode & 0o022 == 0 || owner == 0 && mode & 0o1000 != 0)
}

fn require_local_filesystem(directory: &File, path: &Path) -> Result<(), StoreError> {
    let stat = fs::fstatfs(directory)?;
    // Linux NFS, SMB/CIFS, 9P, AFS, Ceph, CODA, and NCP filesystem signatures.
    if matches!(
        stat.f_type as u32,
        0x6969
            | 0x517b
            | 0xff53_4d42
            | 0xfe53_4d42
            | 0x0102_1997
            | 0x5346_414f
            | 0x00c3_6400
            | 0x7375_7245
            | 0x564c
    ) {
        return Err(StoreError::path(path, "SQLite WAL requires local storage"));
    }
    Ok(())
}
