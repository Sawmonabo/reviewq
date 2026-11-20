//! Owner-private storage for the browser's bounded authorization ledger.
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use reviewq_app::access::AppError;
use reviewq_transport::{BrowserSessionGrants, BrowserSessions, ServiceEndpoint};

use super::{Failure, private_file};
use crate::install::paths;

pub(super) fn open(profile: &Path, endpoint: &ServiceEndpoint) -> Result<BrowserSessions, Failure> {
    let profile = profile.to_owned();
    let database = profile.join("reviewq.sqlite3");
    let file = profile.join("browser-sessions.json");
    let directory_id = file_id(&profile)?;
    let database_id = file_id(&database)?;
    let saved = load(&file)?;
    let save = move |grants: &BrowserSessionGrants| -> Result<(), Failure> {
        if file_id(&profile)? != directory_id || file_id(&database)? != database_id {
            return Err("browser session profile changed".into());
        }
        if paths::exists(&file)? {
            private_file(&file)?;
        }
        paths::write_json(&file, grants)
    };
    Ok(BrowserSessions::restore(
        endpoint,
        directory_id,
        database_id,
        saved,
        move |grants| save(grants).map_err(|_| AppError::Internal),
    )?)
}

fn file_id(path: &Path) -> Result<(u64, u64), Failure> {
    let metadata = fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn load(path: &Path) -> Result<Option<BrowserSessionGrants>, Failure> {
    if !paths::exists(path)? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    // 128 encoded SHA-256 verifiers and their deadlines fit within 16 KiB.
    const LIMIT: u64 = 16 * 1024;
    private_file(path)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Ok(None);
    }
    Ok(serde_json::from_slice(&bytes).ok())
}
