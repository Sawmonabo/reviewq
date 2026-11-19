use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::history::{FileContentRef, NativeContent};
use reviewq_domain::identity::SessionKey;
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use super::{file, locator::Source, records};

const MAX_STREAMS: usize = 4;
const BLOCK: usize = 16 * 1024;
const RETAIN_FOR: Duration = Duration::from_secs(20);

type Key = (SessionKey, FileContentRef);

#[derive(Default)]
pub(super) struct Streams {
    active: Mutex<HashMap<Key, Slot>>,
}

struct Slot {
    touched: Instant,
    read: Arc<AsyncMutex<Option<Read>>>,
}

impl Streams {
    pub(super) async fn read(
        &self,
        session: SessionKey,
        reference: FileContentRef,
        source: Source,
        offset: u64,
        limit: usize,
    ) -> Result<NativeContent, AppError> {
        let key = (session, reference.clone());
        let slot = self.slot(key)?;
        let mut current = slot.lock().await;
        let can_continue = current.as_ref().is_some_and(|read| {
            read.at == offset
                || read
                    .previous
                    .as_ref()
                    .is_some_and(|prev| prev.0 == offset && prev.1 == limit)
        });
        if !can_continue {
            current.take();
            *current = Some(Read::start(source, reference, offset).await?);
        }
        let read = current.as_mut().ok_or(AppError::Internal)?;
        let result = read.next(offset, limit).await;
        if result.is_err() {
            current.take();
        }
        result
    }

    fn slot(&self, key: Key) -> Result<Arc<AsyncMutex<Option<Read>>>, AppError> {
        let mut active = self.active.lock().map_err(|_| AppError::Internal)?;
        active.retain(|_, slot| {
            slot.touched.elapsed() < RETAIN_FOR || Arc::strong_count(&slot.read) > 1
        });
        if !active.contains_key(&key) && active.len() >= MAX_STREAMS {
            let oldest = active
                .iter()
                .filter(|(_, slot)| Arc::strong_count(&slot.read) == 1)
                .min_by_key(|(_, slot)| slot.touched)
                .map(|(key, _)| key.clone())
                .ok_or(AppError::Busy)?;
            active.remove(&oldest);
        }
        let slot = active.entry(key).or_insert_with(|| Slot {
            touched: Instant::now(),
            read: Arc::new(AsyncMutex::new(None)),
        });
        slot.touched = Instant::now();
        Ok(slot.read.clone())
    }
}

struct Read {
    generation: String,
    at: u64,
    length: u64,
    buffer: Vec<u8>,
    output: mpsc::Receiver<Result<Vec<u8>, AppError>>,
    previous: Option<(u64, usize, NativeContent)>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Read {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Read {
    async fn start(
        source: Source,
        reference: FileContentRef,
        offset: u64,
    ) -> Result<Self, AppError> {
        let generation = reference.generation.clone();
        let (metadata, ready) = oneshot::channel();
        let (publish, output) = mpsc::channel(2);
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut guard = CancelOnDrop(Some(cancelled.clone()));
        let cancel = cancelled.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let prepared = match file::content_source(&source, &reference, &cancel) {
                Ok(prepared) if offset <= prepared.bytes => prepared,
                Ok(_) => {
                    let _ = metadata.send(Err(AppError::InvalidRequest));
                    return;
                }
                Err(error) => {
                    let _ = metadata.send(Err(error));
                    return;
                }
            };
            if metadata.send(Ok(prepared.bytes)).is_err() {
                return;
            }
            let mut write = Writer {
                send: publish,
                runtime,
                buffer: Vec::with_capacity(BLOCK),
                skip: offset,
                seen: 0,
                maximum: prepared.bytes,
            };
            let input = file::input(
                &source.file,
                reference.position.offset,
                prepared.end,
                &cancel,
            );
            let observed = records::stream_body(
                source.provider,
                input,
                &source.native_id,
                source.root_session.as_ref(),
                prepared.field,
                &mut write,
            );
            if observed.is_some_and(|record| record.id == reference.entry)
                && write.seen == prepared.bytes
                && file::verify_content(&source, &reference).is_ok()
            {
                let _ = write.flush();
            } else {
                let _ = write.send(Err(AppError::CursorExpired));
            }
        });
        let length = ready.await.map_err(|_| AppError::Unavailable)??;
        guard.0.take();
        Ok(Self {
            generation,
            at: offset,
            length,
            buffer: Vec::new(),
            output,
            previous: None,
            cancelled,
        })
    }

    async fn next(&mut self, offset: u64, limit: usize) -> Result<NativeContent, AppError> {
        if let Some((from, prior_limit, response)) = &self.previous
            && (*from, *prior_limit) == (offset, limit)
        {
            return Ok(response.clone());
        }
        let requested = (self.length - self.at).min(limit as u64) as usize;
        while self.buffer.len() < requested {
            let part = self.output.recv().await.ok_or(AppError::Unavailable)??;
            self.buffer.extend(part);
        }
        let prefix = file::readable_prefix(&self.buffer[..requested])?;
        if requested > 0 && prefix.is_empty() {
            return Err(AppError::CursorExpired);
        }
        let count = prefix.len();
        let text = prefix.to_owned();
        self.buffer.drain(..count);
        self.at += count as u64;
        let reply = NativeContent {
            generation: self.generation.clone(),
            text,
            next_offset: (self.at < self.length).then_some(self.at),
        };
        self.previous = Some((offset, limit, reply.clone()));
        Ok(reply)
    }
}

struct CancelOnDrop(Option<Arc<AtomicBool>>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = &self.0 {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

struct Writer {
    send: mpsc::Sender<Result<Vec<u8>, AppError>>,
    runtime: tokio::runtime::Handle,
    buffer: Vec<u8>,
    skip: u64,
    seen: u64,
    maximum: u64,
}

impl Writer {
    fn send(&self, bytes: Result<Vec<u8>, AppError>) -> io::Result<()> {
        self.runtime
            .block_on(async { tokio::time::timeout(RETAIN_FOR, self.send.send(bytes)).await })
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "history reader timed out"))?
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "history reader closed"))
    }
}

impl Write for Writer {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let size = bytes.len();
        self.seen = self
            .seen
            .checked_add(size as u64)
            .filter(|total| *total <= self.maximum)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "history body changed"))?;
        let skip = self.skip.min(size as u64) as usize;
        self.skip -= skip as u64;
        bytes = &bytes[skip..];
        while !bytes.is_empty() {
            if self.buffer.len() == BLOCK {
                self.flush()?;
            }
            let length = bytes.len().min(BLOCK - self.buffer.len());
            self.buffer.extend_from_slice(&bytes[..length]);
            bytes = &bytes[length..];
        }
        Ok(size)
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let bytes = std::mem::replace(&mut self.buffer, Vec::with_capacity(BLOCK));
            self.send(Ok(bytes))?;
        }
        Ok(())
    }
}
