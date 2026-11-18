//! Owner-marked staging for setup, verification copies and online backups.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::process::geteuid;
use tempfile::{Builder, TempDir};

use super::profile::Profile;
use crate::StoreError;

const PREFIX: &str = ".reviewq-stage-";
const MARKER: &str = "owner";
const MARKER_HEADER: &[u8] = b"reviewq-sqlite-stage-v1\0";
const DATABASE: &str = "profile.sqlite3";
const WAL: &str = "profile.sqlite3-wal";
const DATABASE_FILES: [&str; 4] = [
    DATABASE,
    WAL,
    "profile.sqlite3-shm",
    "profile.sqlite3-journal",
];

pub(super) struct Stage {
    root: TempDir,
    pub(super) database: PathBuf,
}

impl Stage {
    pub(super) fn new(profile: &Profile) -> Result<Self, StoreError> {
        let root = Builder::new()
            .prefix(PREFIX)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(profile.parent())?;
        let root_metadata = fs::symlink_metadata(root.path())?;
        let mut file = create_private(&root.path().join(MARKER))?;
        file.write_all(&marker(&root_metadata, &profile.directory_metadata()?))?;
        file.sync_all()?;
        File::open(root.path())?.sync_all()?;
        let database = root.path().join(DATABASE);
        Ok(Self { root, database })
    }

    pub(super) fn wal(&self) -> PathBuf {
        self.root.path().join(WAL)
    }
}

pub(super) fn create_private(path: &Path) -> Result<File, StoreError> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

/// Requires the enclosing profile lock and leaves unrecognized entries intact.
pub(super) fn reclaim(profile: &Profile) -> Result<(), StoreError> {
    let parent = profile.directory_metadata()?;
    for item in fs::read_dir(profile.parent())? {
        let item = item?;
        if !item
            .file_name()
            .as_encoded_bytes()
            .starts_with(PREFIX.as_bytes())
            || !item.file_type()?.is_dir()
        {
            continue;
        }
        let path = item.path();
        let metadata = item.metadata()?;
        if metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o7777 != 0o700 {
            continue;
        }
        if !recognized(&path, &marker(&metadata, &parent))? {
            continue;
        }
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn marker(stage: &Metadata, parent: &Metadata) -> Vec<u8> {
    let mut data = MARKER_HEADER.to_vec();
    for value in [parent.dev(), parent.ino(), stage.dev(), stage.ino()] {
        data.extend_from_slice(&value.to_be_bytes());
    }
    data
}

fn recognized(path: &Path, expected: &[u8]) -> Result<bool, StoreError> {
    let mut has_marker = false;
    for item in fs::read_dir(path)? {
        let item = item?;
        let name = item.file_name();
        if name != MARKER && !DATABASE_FILES.iter().any(|file| name == *file) {
            return Ok(false);
        }
        if !item.file_type()?.is_file() {
            return Ok(false);
        }
        let metadata = item.metadata()?;
        if metadata.uid() != geteuid().as_raw()
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
        {
            return Ok(false);
        }
        if name == MARKER {
            let mut bytes = Vec::new();
            File::open(item.path())?
                .take(expected.len() as u64 + 1)
                .read_to_end(&mut bytes)?;
            has_marker = bytes == expected;
        }
    }
    Ok(has_marker)
}
