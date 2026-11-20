use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::AppError;

use super::{BLOCK, FileInput, block_size, io_error};

/// Reuse a native byte buffer between adjacent short records without retaining decoded bodies.
pub(in crate::history) struct Lines<'a> {
    file: &'a File,
    cancel: &'a AtomicBool,
    buffer: [u8; BLOCK],
    at: u64,
    length: usize,
}

impl<'a> Lines<'a> {
    pub(in crate::history) fn new(file: &'a File, cancel: &'a AtomicBool) -> Self {
        Self {
            file,
            cancel,
            buffer: [0; BLOCK],
            at: 0,
            length: 0,
        }
    }

    pub(in crate::history) fn next(
        &mut self,
        mut start: u64,
        end: u64,
        budget: &mut u64,
    ) -> Result<u64, AppError> {
        while start < end {
            let limit = block_size(end - start, budget, self.cancel)?;
            if !self.contains(start) {
                self.load(start, limit)?;
            }
            let index = (start - self.at) as usize;
            let bytes = &self.buffer[index..self.length.min(index + limit)];
            if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
                let step = newline as u64 + 1;
                *budget -= step;
                return Ok(start + step);
            }
            let read = bytes.len() as u64;
            *budget -= read;
            start += read;
        }
        Ok(end)
    }

    pub(in crate::history) fn previous(
        &mut self,
        mut end: u64,
        budget: &mut u64,
    ) -> Result<u64, AppError> {
        while end != 0 {
            let limit = block_size(end, budget, self.cancel)?;
            if !self.contains(end - 1) {
                self.load(end - limit as u64, limit)?;
            }
            let after = (end - self.at) as usize;
            let start = after.saturating_sub(limit);
            let bytes = &self.buffer[start..after];
            let read = bytes.len() as u64;
            if let Some(newline) = bytes.iter().rposition(|byte| *byte == b'\n') {
                let step = read - newline as u64 - 1;
                *budget -= step + 1;
                return Ok(end - step);
            }
            *budget -= read;
            end -= read;
        }
        Ok(0)
    }

    pub(in crate::history) fn input(&self, start: u64, end: u64) -> impl Read + '_ {
        if self.contains(start) && end <= self.at + self.length as u64 {
            Input::Buffered {
                bytes: &self.buffer[(start - self.at) as usize..(end - self.at) as usize],
                cancel: self.cancel,
            }
        } else {
            Input::File(FileInput {
                file: self.file,
                offset: start,
                end,
                cancel: self.cancel,
            })
        }
    }

    fn contains(&self, byte: u64) -> bool {
        byte >= self.at && byte - self.at < self.length as u64
    }

    fn load(&mut self, at: u64, length: usize) -> Result<(), AppError> {
        self.file
            .read_exact_at(&mut self.buffer[..length], at)
            .map_err(io_error)?;
        self.at = at;
        self.length = length;
        Ok(())
    }
}

enum Input<'a> {
    Buffered {
        bytes: &'a [u8],
        cancel: &'a AtomicBool,
    },
    File(FileInput<'a>),
}
impl Read for Input<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Buffered { bytes, cancel } => {
                if cancel.load(Ordering::Relaxed) {
                    return Err(io::Error::other("native history read cancelled"));
                }
                bytes.read(buffer)
            }
            Self::File(file) => file.read(buffer),
        }
    }
}
