use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::history::search::index::{CandidateIndex, CandidateWriter, SearchIndex};
use reviewq_app::history::search::{HistorySearch, SourceObservation};
use reviewq_app::reading::{FoldedText, displayed_markdown};
use reviewq_domain::session::SessionTarget;
use sha2::{Digest, Sha256};

use super::{
    Body, DECODE_BLOCK, Lines, Source, ToolProof, bodies, check_cancel, file, records,
    verify_observation,
};

const MIN_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const BUILD_TIME: Duration = Duration::from_secs(15);
const REBUILD_QUIET: Duration = Duration::from_secs(2);
const REBUILD_COOLDOWN: Duration = Duration::from_secs(8);
const CARRY: usize = 256;

pub(in crate::history) struct Acceleration {
    store: Arc<dyn SearchIndex>,
    interest: Mutex<Option<Interest>>,
}

struct Interest {
    key: [u8; 32],
    owner: SessionTarget,
    seen: Instant,
    attempted: Option<Instant>,
    build: Option<Arc<AtomicBool>>,
}

impl Acceleration {
    pub(in crate::history) fn new(store: Arc<dyn SearchIndex>) -> Self {
        Self {
            store,
            interest: Mutex::default(),
        }
    }

    pub(super) fn use_index(
        &self,
        source: &Source,
        owner: &SessionTarget,
        observation: &SourceObservation,
        query: &HistorySearch,
    ) -> Option<Box<dyn CandidateIndex>> {
        let bytes = observation.high_water;
        let key = key(owner, observation);
        let literal = query.literal.folded();
        let length = literal
            .char_indices()
            .nth(3)
            .map_or(literal.len(), |(byte, _)| byte);
        let trigram = &literal[..length];
        let eligible = trigram.chars().count() == 3 && !trigram.contains('\0');
        let current = eligible.then(|| self.store.reader(key, trigram)).flatten();
        let mut interest = self.interest.lock().ok()?;
        if interest.as_ref().is_none_or(|prior| prior.key != key) {
            let previous = interest.take();
            let attempted = previous
                .as_ref()
                .filter(|old| old.owner == *owner)
                .and_then(|old| old.attempted);
            if let Some(stop) = previous.and_then(|old| old.build) {
                stop.store(true, Ordering::Relaxed);
            }
            *interest = Some(Interest {
                key,
                owner: owner.clone(),
                seen: Instant::now(),
                attempted,
                build: None,
            });
            return current;
        }
        if query.from.is_none() && bytes >= MIN_SOURCE_BYTES && current.is_none() && eligible {
            let interest = interest.as_mut()?;
            if interest.attempted.is_some_and(|last| {
                last.elapsed() < REBUILD_COOLDOWN || interest.seen.elapsed() < REBUILD_QUIET
            }) {
                return current;
            }
            if interest
                .build
                .as_ref()
                .is_some_and(|job| job.load(Ordering::Relaxed))
            {
                interest.build.take();
            }
            if interest.build.is_none() {
                let copied = Source {
                    file: source.file.try_clone().ok()?,
                    provider: source.provider,
                    native_id: source.native_id.clone(),
                    fingerprint: source.fingerprint,
                    root_session: source.root_session.clone(),
                    path: source.path.clone(),
                };
                let store = self.store.clone();
                let observation = observation.clone();
                let stop = Arc::new(AtomicBool::new(false));
                interest.attempted = Some(Instant::now());
                interest.build = Some(stop.clone());
                tokio::task::spawn_blocking(move || {
                    if let Ok(writer) = store.writer(key) {
                        let _ = build(&copied, &observation, writer, &stop);
                    }
                    stop.store(true, Ordering::Relaxed);
                });
            }
        }
        current
    }
}

impl Drop for Acceleration {
    fn drop(&mut self) {
        if let Ok(interest) = self.interest.get_mut()
            && let Some(stop) = interest.as_ref().and_then(|item| item.build.as_ref())
        {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

fn key(owner: &SessionTarget, observation: &SourceObservation) -> [u8; 32] {
    let mut digest = Sha256::new();
    // Runtime-disposable FTS5 Unicode projection and tokenizer format.
    digest.update(b"reviewq-reading-fts5-trigram-v1\0");
    for field in [owner.run_id().as_str(), owner.session_key().as_str()] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    digest.update(observation.version);
    digest.update(observation.high_water.to_be_bytes());
    digest.finalize().into()
}

fn build(
    source: &Source,
    observation: &SourceObservation,
    mut index: Box<dyn CandidateWriter>,
    cancel: &AtomicBool,
) -> Result<(), AppError> {
    let complete = observation.high_water;
    let mut lines = Lines::new(&source.file, cancel);
    let mut proof = ToolProof::new(&source.file, cancel);
    let mut start = 0;
    let mut budget = MAX_INPUT_BYTES;
    let deadline = Instant::now() + BUILD_TIME;
    while start < complete && budget > 0 && Instant::now() < deadline {
        check_cancel(cancel)?;
        let end = match lines.next(start, complete, &mut budget) {
            Ok(end) => end,
            Err(AppError::PayloadTooLarge) => break,
            Err(error) => return Err(error),
        };
        index.begin_record(start)?;
        let record = records::project_search(
            source.provider,
            lines.input(start, end),
            &source.native_id,
            source.root_session.as_ref(),
        );
        let certified = match record {
            Ok(Some(mut record)) => {
                proof.redact(source, start, &mut record);
                let mut certified = true;
                match bodies(source, &record) {
                    Ok(bodies) => {
                        for body in bodies {
                            let projection = BodyProjection {
                                source,
                                input: lines.input(start, end),
                                body,
                                record: &record.id,
                            };
                            match projection.cover(index.as_mut()) {
                                Ok(complete) => certified &= complete,
                                Err(AppError::PayloadTooLarge) => {
                                    check_cancel(cancel)?;
                                    verify_observation(source, observation)?;
                                    return index.publish();
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    }
                    Err(_) => certified = false,
                }
                certified
            }
            Ok(None) => true,
            Err(_) => false,
        };
        if let Err(error) = index.commit_record(end, certified) {
            if error == AppError::PayloadTooLarge {
                break;
            }
            return Err(error);
        }
        start = end;
    }
    check_cancel(cancel)?;
    verify_observation(source, observation)?;
    index.publish()
}

struct BodyProjection<'a, R> {
    source: &'a Source,
    input: R,
    body: Body,
    record: &'a reviewq_domain::identity::NativeInputId,
}

impl<R: io::Read> BodyProjection<'_, R> {
    fn cover(self, index: &mut dyn CandidateWriter) -> Result<bool, AppError> {
        let mark = self.body.markdown;
        let mut sink = Chunks {
            index,
            bytes: Vec::new(),
            mark,
            error: None,
            total: 0,
            eligible: true,
        };
        let record = records::stream_body(
            self.source.provider,
            self.input,
            &self.source.native_id,
            self.source.root_session.as_ref(),
            self.body.field,
            &mut sink,
        );
        if let Some(error) = sink.error {
            return Err(error);
        }
        if !record.is_some_and(|record| &record.id == self.record) || sink.total != self.body.bytes
        {
            return Ok(false);
        }
        if mark {
            let Some(display) = file::readable_prefix(&sink.bytes)
                .ok()
                .and_then(displayed_markdown)
            else {
                return Ok(false);
            };
            sink.bytes = display.into_bytes();
        }
        sink.flush_chunk(true)?;
        Ok(sink.eligible)
    }
}

struct Chunks<'a> {
    index: &'a mut dyn CandidateWriter,
    bytes: Vec<u8>,
    mark: bool,
    error: Option<AppError>,
    total: u64,
    eligible: bool,
}

impl Chunks<'_> {
    fn flush_chunk(&mut self, final_chunk: bool) -> Result<(), AppError> {
        let text = file::readable_prefix(&self.bytes)?;
        let folded = FoldedText::new(text);
        if folded.as_str().contains('\0') {
            self.eligible = false;
        }
        if !folded.as_str().is_empty() && self.eligible {
            self.index.folded_chunk(folded.as_str())?;
        }
        let cut = if final_chunk {
            text.len()
        } else {
            folded.settled_prefix(CARRY).byte
        };
        if cut == 0 && self.bytes.len() >= 64 * 1024 {
            self.eligible = false;
            self.bytes.clear();
        } else {
            self.bytes.drain(..cut);
        }
        Ok(())
    }
}

impl Write for Chunks<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.total += bytes.len() as u64;
        if self.eligible {
            self.bytes.extend_from_slice(bytes);
            if !self.mark
                && self.bytes.len() >= DECODE_BLOCK
                && let Err(error) = self.flush_chunk(false)
            {
                self.error = Some(error);
                return Err(io::Error::other(error));
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
