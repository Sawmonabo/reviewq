use std::fs::{self, DirBuilder, Metadata, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use reviewq_app::access::AppError;
use reviewq_app::reports::{DRAWING_RECIPE, DrawingOutput, MAX_DRAWING_BYTES};
use reviewq_domain::identity::ReportHash;
use sha2::{Digest, Sha256};

const DIRECTORY: &str = "diagram-cache";
const PREFIX: &str = "drawing-";
const STAGE: &str = ".drawing-stage-";
const MAX_FILES: usize = 16;
const MAX_BYTES: usize = 1024 * 1024;
const RETAIN: Duration = Duration::from_secs(120);

pub(super) struct Disk {
    root: PathBuf,
    ready: Mutex<Vec<Saved>>,
}

struct Saved {
    key: Key,
    bytes: usize,
    published: SystemTime,
}

struct Key {
    source: ReportHash,
    font: ReportHash,
    svg: ReportHash,
}

impl Key {
    fn filename(&self) -> String {
        format!(
            "{PREFIX}{DRAWING_RECIPE}_{}_{}_{}.svg",
            self.source, self.font, self.svg
        )
    }

    fn parse(filename: &str) -> Option<Self> {
        let parts = filename
            .strip_prefix(PREFIX)?
            .strip_prefix(DRAWING_RECIPE)?
            .strip_prefix('_')?
            .strip_suffix(".svg")?;
        let (source, suffix) = parts.split_once('_')?;
        let (font, svg) = suffix.split_once('_')?;
        Some(Self {
            source: source.parse().ok()?,
            font: font.parse().ok()?,
            svg: svg.parse().ok()?,
        })
    }
}

impl Disk {
    pub(super) fn open(profile: &Path, create: bool) -> Option<Self> {
        let path = profile.join(DIRECTORY);
        if create
            && fs::symlink_metadata(&path).is_err_and(|error| error.kind() == ErrorKind::NotFound)
        {
            return Some(Self {
                root: path,
                ready: Mutex::new(Vec::new()),
            });
        }
        let root = private_directory(path, false)?;
        let mut ready = Vec::new();
        for item in fs::read_dir(&root).ok()? {
            let item = item.ok()?;
            let name = item.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(PREFIX) && !name.starts_with(STAGE) {
                continue;
            }
            let key = Key::parse(name);
            let Some(saved) = key.and_then(|key| inspect(&item.path(), key)) else {
                fs::remove_file(item.path()).ok()?;
                continue;
            };
            if let Some(index) = ready.iter().position(|previous: &Saved| {
                previous.key.source == saved.key.source && previous.key.font == saved.key.font
            }) {
                if ready[index].published >= saved.published {
                    discard(&root, &saved).ok()?;
                    continue;
                }
                discard(&root, &ready[index]).ok()?;
                ready.remove(index);
            }
            ready.push(saved);
            ready.sort_by_key(|saved| saved.published);
            trim(&root, &mut ready, 0, 0).ok()?;
        }
        Some(Self {
            root,
            ready: Mutex::new(ready),
        })
    }

    pub(super) fn font(&self, hash: &ReportHash) -> Result<Option<ReportHash>, AppError> {
        let ready = self.ready.try_lock().map_err(|_| AppError::Unavailable)?;
        Ok(ready
            .iter()
            .rev()
            .find(|saved| !saved.expired() && saved.key.source == *hash)
            .map(|saved| saved.key.font.clone()))
    }

    pub(super) fn publish(
        &self,
        source: ReportHash,
        output: DrawingOutput,
    ) -> Result<(), AppError> {
        let bytes = output.svg.len();
        if bytes > MAX_DRAWING_BYTES || !super::svg::valid(&output.svg) {
            return Err(AppError::InvalidRequest);
        }
        let key = Key {
            source,
            font: output.font,
            svg: checksum(&output.svg),
        };
        let mut ready = self.ready.lock().map_err(|_| AppError::Internal)?;
        if ready.iter().any(|saved| {
            !saved.expired() && saved.key.source == key.source && saved.key.font == key.font
        }) {
            return Ok(());
        }
        trim(&self.root, &mut ready, 1, bytes)?;
        let root = private_directory(self.root.clone(), true).ok_or(AppError::Unavailable)?;
        let mut stage = tempfile::Builder::new()
            .prefix(STAGE)
            .tempfile_in(root)
            .map_err(|_| AppError::Unavailable)?;
        stage
            .write_all(output.svg.as_bytes())
            .map_err(|_| AppError::Unavailable)?;
        stage
            .as_file()
            .sync_all()
            .map_err(|_| AppError::Unavailable)?;
        let published = stage
            .as_file()
            .metadata()
            .and_then(|meta| meta.modified())
            .map_err(|_| AppError::Unavailable)?;
        stage
            .persist(self.root.join(key.filename()))
            .map_err(|_| AppError::Unavailable)?;
        ready.push(Saved {
            key,
            bytes,
            published,
        });
        Ok(())
    }

    pub(super) fn read(&self, source: &ReportHash, font: &ReportHash) -> Result<String, AppError> {
        let mut ready = self.ready.lock().map_err(|_| AppError::Internal)?;
        let index = ready
            .iter()
            .position(|saved| {
                !saved.expired() && saved.key.source == *source && saved.key.font == *font
            })
            .ok_or(AppError::NotFound)?;
        let saved = &ready[index];
        let svg = read(&self.root.join(saved.key.filename()))
            .ok()
            .filter(|(text, _)| checksum(text) == saved.key.svg && super::svg::valid(text));
        let Some((text, _)) = svg else {
            discard(&self.root, saved)?;
            ready.remove(index);
            return Err(AppError::NotFound);
        };
        let saved = ready.remove(index);
        ready.push(saved);
        Ok(text)
    }

    pub(super) fn expire(&self) -> Result<(), AppError> {
        let mut ready = self.ready.lock().map_err(|_| AppError::Internal)?;
        trim(&self.root, &mut ready, 0, 0)
    }
}

impl Saved {
    fn expired(&self) -> bool {
        self.published
            .elapsed()
            .map_or(true, |elapsed| elapsed >= RETAIN)
    }
}

fn trim(root: &Path, ready: &mut Vec<Saved>, reserve: usize, bytes: usize) -> Result<(), AppError> {
    let mut at = 0;
    while at < ready.len() {
        if ready[at].expired() {
            discard(root, &ready[at])?;
            ready.remove(at);
        } else {
            at += 1;
        }
    }
    let mut retained = ready.iter().map(|entry| entry.bytes).sum::<usize>();
    while ready.len() + reserve > MAX_FILES || retained + bytes > MAX_BYTES {
        discard(root, &ready[0])?;
        retained -= ready.remove(0).bytes;
    }
    Ok(())
}

fn discard(root: &Path, saved: &Saved) -> Result<(), AppError> {
    match fs::remove_file(root.join(saved.key.filename())) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AppError::Unavailable),
    }
}

fn inspect(path: &Path, key: Key) -> Option<Saved> {
    let (svg, published) = read(path).ok()?;
    let saved = Saved {
        key,
        bytes: svg.len(),
        published,
    };
    (!saved.expired() && checksum(&svg) == saved.key.svg && super::svg::valid(&svg))
        .then_some(saved)
}

fn read(path: &Path) -> std::io::Result<(String, SystemTime)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || !private(&metadata)
        || metadata.nlink() != 1
        || metadata.len() > MAX_DRAWING_BYTES as u64
    {
        return Err(ErrorKind::InvalidData.into());
    }
    let mut svg = String::new();
    file.take(MAX_DRAWING_BYTES as u64 + 1)
        .read_to_string(&mut svg)?;
    if svg.len() > MAX_DRAWING_BYTES {
        return Err(ErrorKind::InvalidData.into());
    }
    Ok((svg, metadata.modified()?))
}

pub(super) fn private_directory(path: PathBuf, create: bool) -> Option<PathBuf> {
    if create {
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    let info = fs::symlink_metadata(&path).ok()?;
    (info.is_dir() && private(&info)).then_some(path)
}

fn private(info: &Metadata) -> bool {
    info.uid() == rustix::process::getuid().as_raw() && info.mode() & 0o077 == 0
}

fn checksum(source: &str) -> ReportHash {
    ReportHash::from_sha256(Sha256::digest(source.as_bytes()).into())
}
