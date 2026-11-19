use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::AppError;
use reviewq_app::history::{NativeTask, TaskStatus};
use reviewq_domain::session::{HostProvider, NativeBinding};
use rustix::fs::{self, Dir, Mode, OFlags, ResolveFlags};
use serde::Deserialize;

use super::locator::Source;

const MAX_FILE: u64 = 16 * 1024;
const MAX_TASKS: usize = 100;

pub(super) fn read(
    binding: &NativeBinding,
    source: &Source,
    cancel: &AtomicBool,
) -> Result<Option<Vec<NativeTask>>, AppError> {
    if source.provider != HostProvider::Claude || source.root_session.is_some() {
        return Ok(None);
    }
    let session = source.native_id.as_str();
    let exact = uuid::Uuid::parse_str(session).map_err(|_| AppError::Unavailable)?;
    if exact.hyphenated().to_string() != session || exact.is_nil() {
        return Err(AppError::Unavailable);
    }
    let root = open_path(binding.identity().profile().as_path())?;
    let dir = match fs::openat2(
        root,
        Path::new("tasks").join(session),
        OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    ) {
        Ok(dir) => File::from(dir),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(AppError::Unavailable),
    };
    let owner = rustix::process::getuid().as_raw();
    let prior = dir.metadata().map_err(|_| AppError::Unavailable)?;
    if prior.uid() != owner {
        return Err(AppError::Forbidden);
    }
    let mut tasks = Vec::new();
    let mut inspected = 0;
    for item in Dir::read_from(&dir).map_err(|_| AppError::Unavailable)? {
        inspected += 1;
        if inspected > MAX_TASKS * 2 {
            return Err(AppError::PayloadTooLarge);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::Unavailable);
        }
        let item = item.map_err(|_| AppError::Unavailable)?;
        let Ok(name) = item.file_name().to_str() else {
            continue;
        };
        let Some(id) = name
            .strip_suffix(".json")
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        if id == 0 || name != format!("{id}.json") {
            continue;
        }
        if tasks.len() >= MAX_TASKS {
            return Err(AppError::PayloadTooLarge);
        }
        let file = File::from(
            fs::openat(
                &dir,
                item.file_name(),
                OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| AppError::Unavailable)?,
        );
        let first = file.metadata().map_err(|_| AppError::Unavailable)?;
        if !first.is_file() || first.uid() != owner || first.len() > MAX_FILE {
            return Err(AppError::Unavailable);
        }
        let mut bytes = Vec::new();
        (&file)
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AppError::Unavailable)?;
        let last = file.metadata().map_err(|_| AppError::Unavailable)?;
        if bytes.len() as u64 != first.len() || changed(&first, &last) {
            return Err(AppError::Unavailable);
        }
        let task: Native = serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if task.id != id.to_string()
            || task.subject.is_empty()
            || task.subject.len() > 256
            || task.subject.chars().any(char::is_control)
        {
            return Err(AppError::Unavailable);
        }
        let status = match task.status {
            Status::Pending => TaskStatus::Pending,
            Status::InProgress => TaskStatus::InProgress,
            Status::Completed => TaskStatus::Completed,
        };
        tasks.push((
            id,
            NativeTask {
                id: task.id,
                title: task.subject,
                status,
            },
        ));
    }
    if changed(&prior, &dir.metadata().map_err(|_| AppError::Unavailable)?) {
        return Err(AppError::Unavailable);
    }
    tasks.sort_unstable_by_key(|(id, _)| *id);
    Ok(Some(tasks.into_iter().map(|(_, task)| task).collect()))
}

fn changed(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    (
        a.len(),
        a.mtime(),
        a.mtime_nsec(),
        a.ctime(),
        a.ctime_nsec(),
    ) != (
        b.len(),
        b.mtime(),
        b.mtime_nsec(),
        b.ctime(),
        b.ctime_nsec(),
    )
}

fn open_path(path: &Path) -> Result<File, AppError> {
    fs::openat2(
        fs::CWD,
        path,
        OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .map(File::from)
    .map_err(|_| AppError::Unavailable)
}

#[derive(Deserialize)]
struct Native {
    id: String,
    subject: String,
    status: Status,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    InProgress,
    Completed,
}
