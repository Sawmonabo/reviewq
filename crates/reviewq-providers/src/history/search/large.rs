use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reviewq_app::access::{AppError, admits_disk_scratch};
use reviewq_app::history::search::SourceObservation;
use reviewq_domain::session::SessionTarget;

use super::{Body, NativeValue, RecordWindow, Source, file, project_body, stream_value};

pub(super) const RECORD_BYTES: u64 = 128 * 1024;
const MAX_STAGED_BYTES: u64 = file::READ_BUDGET;
const IDLE: Duration = Duration::from_secs(20);
static RESERVED: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
pub(in crate::history) struct LargeRecords {
    directory: Option<PathBuf>,
    retained: Option<RetainedRecord>,
}

struct RetainedRecord {
    owner: SessionTarget,
    source: SourceObservation,
    touched: Instant,
    window: RecordWindow,
}

impl LargeRecords {
    pub(in crate::history) fn new(directory: PathBuf) -> Self {
        Self {
            directory: Some(directory),
            retained: None,
        }
    }

    pub(in crate::history) fn expire(&mut self) {
        if self
            .retained
            .as_ref()
            .is_some_and(|record| record.touched.elapsed() >= IDLE)
        {
            self.retained.take();
        }
    }

    pub(super) fn take(
        &mut self,
        owner: &SessionTarget,
        source: &SourceObservation,
        offset: u64,
    ) -> Option<RecordWindow> {
        self.expire();
        let record = self.retained.as_ref()?;
        if &record.owner == owner && &record.source != source {
            self.retained.take();
            return None;
        }
        if &record.owner == owner && record.window.position.offset == offset {
            return self.retained.take().map(|retained| retained.window);
        }
        None
    }

    pub(super) fn remember(
        &mut self,
        owner: &SessionTarget,
        source: &SourceObservation,
        window: RecordWindow,
    ) {
        if window.end - window.position.offset >= RECORD_BYTES {
            self.retained = Some(RetainedRecord {
                owner: owner.clone(),
                source: source.clone(),
                touched: Instant::now(),
                window,
            });
        }
    }

    pub(super) fn stage(
        &mut self,
        source: &Source,
        window: &RecordWindow,
        body: &Body,
        field: Option<&NativeValue>,
        cancel: &AtomicBool,
    ) -> Result<StagedBody, AppError> {
        let directory = self.directory.as_deref().ok_or(AppError::Unavailable)?;
        // Release the preceding record before reserving another active body.
        self.retained.take();
        StagedBody::build(directory, source, window, body, field, cancel)
    }
}

pub(super) struct StagedBody {
    pub(super) key: u16,
    pub(super) file: File,
    _reservation: Reservation,
}

struct Reservation;

impl Drop for Reservation {
    fn drop(&mut self) {
        RESERVED.store(false, Ordering::Release);
    }
}

impl StagedBody {
    fn build(
        directory: &Path,
        source: &Source,
        window: &RecordWindow,
        body: &Body,
        field: Option<&NativeValue>,
        cancel: &AtomicBool,
    ) -> Result<Self, AppError> {
        if body.bytes > MAX_STAGED_BYTES {
            return Err(AppError::PayloadTooLarge);
        }
        RESERVED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AppError::Busy)?;
        let reservation = Reservation;
        let file = tempfile::tempfile_in(directory).map_err(|_| AppError::Unavailable)?;
        let kind = rustix::fs::fstatfs(&file)
            .map_err(|_| AppError::Unavailable)?
            .f_type as u64;
        if !admits_disk_scratch(kind) {
            return Err(AppError::Unavailable);
        }
        let mut writer = BoundedBody {
            output: BufWriter::new(&file),
            remaining: body.bytes,
        };
        let projected = if let Some(field) = field {
            let input = file::input(&source.file, field.span.start, field.span.end, cancel);
            stream_value(input, &mut writer).is_ok_and(|bytes| bytes == body.bytes)
        } else {
            project_body(
                source,
                &window.record.id,
                body,
                file::input(&source.file, window.position.offset, window.end, cancel),
                &mut writer,
            )
        };
        if !projected || writer.remaining != 0 {
            return Err(AppError::Unavailable);
        }
        writer.flush().map_err(|_| AppError::Unavailable)?;
        drop(writer);
        Ok(Self {
            key: body.key(),
            file,
            _reservation: reservation,
        })
    }
}

struct BoundedBody<'a> {
    output: BufWriter<&'a File>,
    remaining: u64,
}

impl Write for BoundedBody<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other("native body length changed"));
        }
        self.output.write_all(bytes)?;
        self.remaining -= bytes.len() as u64;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
