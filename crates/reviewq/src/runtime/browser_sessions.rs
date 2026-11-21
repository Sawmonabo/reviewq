//! Installation-local signing authority for browser cookies.

use std::fs;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use reviewq_transport::{BrowserSessions, ServiceEndpoint};

use super::{Failure, private_file};
use crate::install::paths;

const KEY_FILE: &str = "browser-session.key";

pub(super) fn open(profile: &Path, endpoint: &ServiceEndpoint) -> Result<BrowserSessions, Failure> {
    let directory_id = file_id(profile)?;
    let database_id = file_id(&profile.join("reviewq.sqlite3"))?;
    let key = load_key(&profile.join(KEY_FILE))?;
    Ok(BrowserSessions::new(
        endpoint,
        directory_id,
        database_id,
        key,
    )?)
}

fn file_id(path: &Path) -> Result<(u64, u64), Failure> {
    let metadata = fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn load_key(path: &Path) -> Result<[u8; 32], Failure> {
    let mut key = [0; 32];
    if !paths::exists(path)? {
        getrandom::fill(&mut key)
            .map_err(|_| "Could not generate the browser session signing key.")?;
        if let Err(error) = paths::atomic_file(path, &key, false)
            && !error
                .downcast_ref::<tempfile::PersistError>()
                .is_some_and(|persist| persist.error.kind() == ErrorKind::AlreadyExists)
        {
            return Err(error);
        }
    }

    let mut file = private_file(path)?;
    if file.metadata()?.len() != key.len() as u64 {
        return Err("Invalid browser session signing key length.".into());
    }
    file.read_exact(&mut key)?;
    Ok(key)
}
