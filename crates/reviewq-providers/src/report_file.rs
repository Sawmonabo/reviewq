use std::fs::{self, File, Metadata};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::reports::{CHUNK_BYTES, ReportFileSource};
use reviewq_app::workflow::{NativeCall, ReportContent, WorkflowTool};
use reviewq_domain::session::AbsolutePath;
use rustix::fs::{Mode, OFlags, ResolveFlags};

/// Reads only an exact regular file within the verified native checkout.
pub struct RegisteredReportFiles;

impl ReportFileSource for RegisteredReportFiles {
    fn open_report(
        &self,
        call: NativeCall,
        path: AbsolutePath,
    ) -> AppFuture<'_, Arc<dyn ReportContent>> {
        Box::pin(async move {
            if call.tool != WorkflowTool::Report {
                return Err(AppError::Forbidden);
            }
            tokio::task::spawn_blocking(move || {
                let root = call.lead.binding().cwd().as_path();
                let source = path.as_path();
                let relative = source.strip_prefix(root).map_err(|_| AppError::Forbidden)?;
                if relative.as_os_str().is_empty() {
                    return Err(AppError::InvalidRequest);
                }
                let root_fd = rustix::fs::open(
                    root,
                    OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|_| AppError::Unavailable)?;
                let fd = rustix::fs::openat2(
                    &root_fd,
                    relative,
                    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                    ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
                )
                .map_err(|_| AppError::Unavailable)?;
                let file = File::from(fd);
                let metadata = file.metadata().map_err(|_| AppError::Unavailable)?;
                if !metadata.is_file() || metadata.uid() != rustix::process::getuid().as_raw() {
                    return Err(AppError::Forbidden);
                }
                let state = Stamp::from_metadata(&metadata);
                let content = Content {
                    file: Arc::new(file),
                    path: source.to_path_buf(),
                    state,
                };
                content.verify()?;
                Ok(Arc::new(content) as Arc<dyn ReportContent>)
            })
            .await
            .map_err(|_| AppError::Internal)?
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Stamp {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stamp {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    fn require(self, actual: Metadata) -> Result<(), AppError> {
        if !actual.is_file() || Self::from_metadata(&actual) != self {
            return Err(AppError::Conflict);
        }
        Ok(())
    }
}

struct Content {
    file: Arc<File>,
    path: PathBuf,
    state: Stamp,
}

impl Content {
    fn verify(&self) -> Result<(), AppError> {
        verify(&self.path, &self.file, self.state)
    }
}

fn verify(path: &Path, file: &File, state: Stamp) -> Result<(), AppError> {
    if fs::canonicalize(path).map_err(|_| AppError::Unavailable)? != path {
        return Err(AppError::Conflict);
    }
    state.require(fs::symlink_metadata(path).map_err(|_| AppError::Unavailable)?)?;
    state.require(file.metadata().map_err(|_| AppError::Unavailable)?)
}

impl ReportContent for Content {
    fn byte_length(&self) -> u64 {
        self.state.bytes
    }

    fn read(&self, offset: u64, length: usize) -> AppFuture<'_, Vec<u8>> {
        Box::pin(async move {
            if length == 0
                || length > CHUNK_BYTES
                || offset > self.state.bytes
                || length as u64 > self.state.bytes - offset
            {
                return Err(AppError::InvalidRequest);
            }
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || {
                let mut bytes = vec![0; length];
                file.read_exact_at(&mut bytes, offset)
                    .map_err(|_| AppError::Unavailable)?;
                Ok(bytes)
            })
            .await
            .map_err(|_| AppError::Internal)?
        })
    }

    fn verify_unchanged(&self) -> AppFuture<'_, ()> {
        Box::pin(async move {
            let file = self.file.clone();
            let path = self.path.clone();
            let state = self.state;
            tokio::task::spawn_blocking(move || verify(&path, &file, state))
                .await
                .map_err(|_| AppError::Internal)?
        })
    }
}
