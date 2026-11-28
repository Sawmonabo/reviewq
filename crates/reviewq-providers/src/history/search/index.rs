use std::io::{self, Read, Write};
use std::ops::Range;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::history::FileContentRef;
use reviewq_app::history::document::MAX_INDEX_BYTES;
use reviewq_app::history::search::index::{
    CandidateIndex, CandidateWriter, IndexProof, SearchIndex,
};
use reviewq_app::history::search::{HistorySearch, SourceObservation};
use reviewq_app::reading::{FoldedText, displayed_markdown};
use reviewq_domain::session::SessionTarget;
use sha2::{Digest, Sha256};

use super::{
    Body, DECODE_BLOCK, Documents, Lines, NativeValue, Source, ToolProof, bodies, check_cancel,
    document, file, native_string, project_body, records, stream_value, verify_observation,
};

const MIN_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RECORD_WORK: u64 = 12 * 1024 * 1024;
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
        documents: &Arc<Documents>,
    ) -> Option<Box<dyn CandidateIndex>> {
        let bytes = observation.high_water;
        let source_key = scope(source, owner)?;
        let key = observation_key(source_key, observation);
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
                let owner = owner.clone();
                let documents = documents.clone();
                let stop = Arc::new(AtomicBool::new(false));
                interest.attempted = Some(Instant::now());
                interest.build = Some(stop.clone());
                tokio::task::spawn_blocking(move || {
                    if let Ok(generation) = file::generation(&copied) {
                        let source = IndexedSource {
                            native: &copied,
                            owner: &owner,
                            observation: &observation,
                            generation: &generation,
                            documents: &documents,
                        };
                        let _ = build(source, source_key, key, store.as_ref(), &stop);
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

fn scope(source: &Source, owner: &SessionTarget) -> Option<[u8; 32]> {
    let mut digest = Sha256::new();
    digest.update(b"reviewq-reading-fts5-trigram-v4\0");
    for field in [owner.run_id().as_str(), owner.session_key().as_str()] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    let stamp = source.file.metadata().ok()?;
    digest.update(source.fingerprint);
    digest.update(stamp.dev().to_be_bytes());
    digest.update(stamp.ino().to_be_bytes());
    Some(digest.finalize().into())
}

fn observation_key(scope: [u8; 32], observation: &SourceObservation) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(scope);
    digest.update(observation.version);
    digest.update(observation.high_water.to_be_bytes());
    digest.finalize().into()
}

struct Budget<'a> {
    left: u64,
    deadline: Instant,
    cancel: &'a AtomicBool,
}

impl<'a> Budget<'a> {
    fn check(&self) -> Result<(), AppError> {
        check_cancel(self.cancel)?;
        if Instant::now() >= self.deadline {
            return Err(AppError::Busy);
        }
        Ok(())
    }
    fn charge(&mut self, bytes: u64) -> Result<(), AppError> {
        self.check()?;
        self.left = self.left.checked_sub(bytes).ok_or(AppError::Busy)?;
        Ok(())
    }
    fn guard<R: Read>(&self, read: R) -> Guarded<'a, R> {
        Guarded {
            read,
            stop: self.cancel,
            deadline: self.deadline,
        }
    }
}

struct Guarded<'a, R> {
    read: R,
    stop: &'a AtomicBool,
    deadline: Instant,
}

impl<R: Read> Read for Guarded<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.stop.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
            return Err(io::Error::other("transcript index work ended"));
        }
        self.read.read(bytes)
    }
}

struct IndexedSource<'a> {
    native: &'a Source,
    owner: &'a SessionTarget,
    observation: &'a SourceObservation,
    generation: &'a str,
    documents: &'a Documents,
}

fn build(
    indexed: IndexedSource<'_>,
    scope: [u8; 32],
    key: [u8; 32],
    store: &dyn SearchIndex,
    stop: &AtomicBool,
) -> Result<(), AppError> {
    let source = indexed.native;
    let observation = indexed.observation;
    // Leave room for visibility proof lookbacks and overlapping escape-safe reads.
    let mut work = Budget {
        left: MAX_INPUT_BYTES - 8 * 1024 * 1024,
        deadline: Instant::now() + BUILD_TIME,
        cancel: stop,
    };
    let mut prefix = Sha256::new();
    let previous = store
        .proof(scope)
        .filter(|old| old.high_water < observation.high_water);
    let reuse = if let Some(previous) = previous {
        work.charge(previous.covered_through)?;
        let mut source_bytes =
            work.guard(file::input(&source.file, 0, previous.covered_through, stop));
        io::copy(&mut source_bytes, &mut DigestWriter(&mut prefix))
            .map_err(|_| AppError::Unavailable)?;
        verify_observation(source, observation)?;
        if <[u8; 32]>::from(prefix.clone().finalize()) == previous.prefix_hash {
            Some(previous)
        } else {
            prefix = Sha256::new();
            None
        }
    } else {
        None
    };
    let mut index = store.writer(key, reuse)?;
    let mut capped = reuse.is_some_and(|previous| previous.capped);
    let mut lines = Lines::new(&source.file, stop);
    let mut proof = ToolProof::new(&source.file, stop);
    while !capped
        && index.covered_through() < observation.high_water
        && work.left > 0
        && work.check().is_ok()
    {
        let start = index.covered_through();
        let end = match lines.next(start, observation.high_water, &mut work.left) {
            Ok(end) => end,
            Err(AppError::PayloadTooLarge) => break,
            Err(error) => return Err(error),
        };
        let mut with_record = prefix.clone();
        match index_record(
            &indexed,
            &lines,
            &mut proof,
            start..end,
            &mut work,
            index.as_mut(),
            &mut with_record,
        ) {
            Ok(()) => prefix = with_record,
            Err(AppError::PayloadTooLarge) => capped = true,
            Err(AppError::Busy) => break,
            Err(error) => return Err(error),
        }
    }
    check_cancel(stop)?;
    verify_observation(source, observation)?;
    let covered_through = index.covered_through();
    index.publish(IndexProof {
        source: scope,
        observation: key,
        high_water: observation.high_water,
        covered_through,
        prefix_hash: prefix.finalize().into(),
        capped,
    })
}

fn index_record(
    indexed: &IndexedSource<'_>,
    lines: &Lines<'_>,
    visibility: &mut ToolProof<'_>,
    span: Range<u64>,
    work: &mut Budget<'_>,
    index: &mut dyn CandidateWriter,
    prefix: &mut Sha256,
) -> Result<(), AppError> {
    let source = indexed.native;
    let bytes = span.end - span.start;
    work.charge(bytes)?;
    let mut input = Hashing {
        read: work.guard(lines.input(span.start, span.end)),
        hash: prefix,
    };
    let record = if bytes <= MAX_RECORD_WORK {
        records::project_search(
            source.provider,
            &mut input,
            &source.native_id,
            source.root_session.as_ref(),
        )
    } else {
        Err(AppError::PayloadTooLarge)
    };
    let remainder = io::copy(&mut input, &mut io::sink());
    work.check()?;
    remainder.map_err(|_| AppError::Unavailable)?;
    index.begin_record(span.start)?;
    if record
        .as_ref()
        .is_ok_and(|record| record.as_ref().is_some_and(|record| record.codex_wait))
    {
        // An output can be appended after the wait's completed item. Search
        // these rows directly so a reused prefix cannot skip the new detail.
        return index.commit_record(span.end, false);
    }
    let certified = if let Ok(Some(mut record)) = record {
        visibility.redact(source, span.start, &mut record);
        let mut left = MAX_RECORD_WORK - bytes;
        let mut certified = record.next_part.is_none();
        if let Ok(bodies) = bodies(source, &record) {
            for body in bodies {
                let native = body.source_range(span.clone())?;
                let cost = if body.document() {
                    if body.bytes > MAX_INDEX_BYTES as u64 {
                        certified = false;
                        continue;
                    }
                    // Line discovery and field proof reread at most the record,
                    // followed by one sparse pass over the selected value(s).
                    3 * bytes
                } else {
                    native
                        .as_ref()
                        .map_or(bytes, |field| field.span.end - field.span.start)
                };
                let Some(remaining) = left.checked_sub(cost) else {
                    certified = false;
                    break;
                };
                work.charge(cost)?;
                left = remaining;
                if body.document() {
                    let reference = FileContentRef {
                        generation: indexed.generation.to_owned(),
                        entry: record.id.clone(),
                        part: body.part,
                        position: file::file_position(
                            &source.file,
                            span.start,
                            indexed.observation.high_water,
                        )?,
                    };
                    certified &= cover_document(indexed, &reference, index, work, &mut left)?;
                    continue;
                }
                let projection = BodyProjection {
                    source,
                    record: &record.id,
                    span: span.clone(),
                    body,
                    native,
                };
                certified &= projection.cover(index, work)?;
            }
        } else {
            certified = false;
        }
        certified
    } else {
        matches!(record, Ok(None))
    };
    work.check()?;
    index.commit_record(span.end, certified)
}

struct DigestWriter<'a>(&'a mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Hashing<'a, R> {
    read: R,
    hash: &'a mut Sha256,
}

impl<R: Read> Read for Hashing<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let size = self.read.read(bytes)?;
        self.hash.update(&bytes[..size]);
        Ok(size)
    }
}

fn cover_document(
    source: &IndexedSource<'_>,
    reference: &FileContentRef,
    index: &mut dyn CandidateWriter,
    work: &mut Budget<'_>,
    left: &mut u64,
) -> Result<bool, AppError> {
    let mut offset = 0;
    loop {
        // A window may decode part of both enclosing escape-safe checkpoints.
        let window_cost = 2 * 32 * 1024;
        let Some(remaining) = left.checked_sub(window_cost) else {
            return Ok(false);
        };
        work.charge(window_cost)?;
        *left = remaining;
        let projection = match document::read(
            source.documents,
            source.owner,
            source.native,
            reference,
            offset,
            work.cancel,
            document::Reader::Index,
        ) {
            Ok(projection) => projection,
            Err(AppError::NotFound | AppError::Busy | AppError::Unavailable) => {
                work.check()?;
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        work.check()?;
        if projection.needs_context || projection.literal_body {
            return Ok(false);
        }
        let Some(text) = projection.text.filter(|text| !text.contains('\0')) else {
            return Ok(false);
        };
        if !text.is_empty() {
            let folded = FoldedText::new(&text);
            index.folded_chunk(folded.as_str())?;
        }
        let Some(next) = projection.next else {
            return Ok(true);
        };
        if next <= offset {
            return Ok(false);
        }
        offset = next;
    }
}

struct BodyProjection<'a> {
    source: &'a Source,
    record: &'a reviewq_domain::identity::NativeInputId,
    span: Range<u64>,
    body: Body,
    native: Option<NativeValue>,
}

impl BodyProjection<'_> {
    fn cover(self, index: &mut dyn CandidateWriter, work: &Budget<'_>) -> Result<bool, AppError> {
        let mark = self.body.markdown;
        let mut sink = Chunks {
            index,
            bytes: Vec::new(),
            mark,
            error: None,
            total: 0,
            eligible: true,
        };
        let projected = match self.native {
            Some(field) if field.string => {
                native_string::visit(&self.source.file, field.span, |text| {
                    work.check()?;
                    sink.write_all(text.as_bytes())
                        .map_err(|_| AppError::Unavailable)
                })
                .is_ok()
            }
            Some(field) => {
                let input = work.guard(file::input(
                    &self.source.file,
                    field.span.start,
                    field.span.end,
                    work.cancel,
                ));
                stream_value(input, &mut sink).is_ok_and(|bytes| bytes == self.body.bytes)
            }
            None => {
                let input = work.guard(file::input(
                    &self.source.file,
                    self.span.start,
                    self.span.end,
                    work.cancel,
                ));
                project_body(self.source, self.record, &self.body, input, &mut sink)
            }
        };
        if let Some(error) = sink.error {
            return Err(error);
        }
        work.check()?;
        if !projected || sink.total != self.body.bytes {
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
                self.error = Some(error.clone());
                return Err(io::Error::other(error));
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
