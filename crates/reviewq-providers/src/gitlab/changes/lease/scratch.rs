use std::fs::File;
use std::path::Path;
use std::sync::Mutex;

use reviewq_app::access::admits_disk_scratch;
use reviewq_domain::identity::MrIdentity;

// A decoded selected patch is smaller than its bounded 32 MiB JSON response.
// Reserve the entire possible file before creating it. The counter survives
// GitLab adapter replacement and includes detached responses and decoders.
const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
static RESERVED: Mutex<Vec<ScratchKey>> = Mutex::new(Vec::new());

#[derive(PartialEq, Eq)]
pub(in crate::gitlab::changes) struct ScratchKey {
    pub source: MrIdentity,
    pub revision: String,
    pub ordinal: u32,
}

pub(in crate::gitlab::changes) struct ScratchFile {
    pub file: File,
    _reservation: Reservation,
}

struct Reservation(ScratchKey);

impl ScratchFile {
    pub(in crate::gitlab::changes) fn create(profile: &Path, key: ScratchKey) -> Option<Self> {
        let reservation = Reservation::acquire(key)?;
        if !admits_disk_scratch(rustix::fs::statfs(profile).ok()?.f_type as u64) {
            return None;
        }
        let file = crate::http::private_file(profile).ok()?;
        // Verify the actual open file, including if its directory moved.
        if !admits_disk_scratch(rustix::fs::fstatfs(&file).ok()?.f_type as u64) {
            return None;
        }
        Some(Self {
            file,
            _reservation: reservation,
        })
    }
}

impl Reservation {
    fn acquire(key: ScratchKey) -> Option<Self> {
        let mut entries = RESERVED.lock().ok()?;
        if entries.len() >= MAX_TOTAL_BYTES / MAX_FILE_BYTES || entries.contains(&key) {
            return None;
        }
        entries.push(ScratchKey {
            source: key.source.clone(),
            revision: key.revision.clone(),
            ordinal: key.ordinal,
        });
        Some(Self(key))
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut entries) = RESERVED.lock()
            && let Some(index) = entries.iter().position(|key| key == &self.0)
        {
            entries.swap_remove(index);
        }
    }
}
