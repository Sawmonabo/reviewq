use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use super::lease::{Body, MEMORY_LIMIT, ScratchFile, ScratchKey};
use super::paths::matches_digest;
use super::{file_lookup_key, hex, safe_path};
use crate::http::ResponseReader;
use reviewq_app::access::AppError;
use reviewq_app::changes::{PatchAvailability, PatchFile, PatchMetrics, PatchRead};
use reviewq_domain::identity::MrIdentity;
use sha2::{Digest, Sha256};
use struson::reader::{JsonReader, JsonStreamReader};

mod search;
mod stats;
mod window;
pub(super) use search::find_matches;
use stats::DiffStats;
pub(super) use stats::hunk_ranges;
use window::Capture;

const SMALL_PATCH_BYTES: usize = 32 * 1024;
const SMALL_PATCHES_PER_PAGE_BYTES: usize = 128 * 1024;

enum Retain<'a> {
    Selected(&'a Selection, u32),
    Small(usize),
}

#[derive(Clone)]
pub(super) struct Selection {
    pub source: MrIdentity,
    pub revision: String,
    pub preferred: Option<String>,
    pub default: bool,
    pub read: PatchRead,
    pub policy: BodyPolicy,
}

#[derive(Clone, Copy)]
pub(super) enum BodyPolicy {
    Lease,
    Uncached,
    ReadAhead,
}

#[derive(Default)]
struct Diff {
    old_path: Option<String>,
    new_path: Option<String>,
    renamed: Option<bool>,
    created: Option<bool>,
    deleted: Option<bool>,
    collapsed: bool,
    too_large: bool,
    binary: bool,
}

pub(super) struct DiffPage {
    pub files: Vec<PatchFile>,
    pub selected: Option<(PatchFile, Body)>,
    pub small: Vec<(usize, Arc<Body>)>,
}

pub(super) fn decode(
    wire: ResponseReader,
    profile: &Path,
    first: u32,
    limit: u16,
    selection: &Selection,
) -> Result<DiffPage, AppError> {
    let mut reader = JsonStreamReader::new(wire);
    reader.begin_array().map_err(invalid)?;
    let preferred = selection.preferred.as_deref();
    let preferred_index = preferred.and_then(super::ordinal);
    let mut files = Vec::new();
    let mut selected = None;
    let mut small = Vec::new();
    let mut remaining = SMALL_PATCHES_PER_PAGE_BYTES;
    while reader.has_next().map_err(invalid)? {
        if files.len() >= usize::from(limit) {
            return Err(AppError::Unavailable);
        }
        let index = first + files.len() as u32;
        let primary = preferred_index.map_or(selection.default && selected.is_none(), |value| {
            value == index
        });
        let retain = if primary {
            Retain::Selected(selection, index)
        } else {
            Retain::Small(remaining.min(SMALL_PATCH_BYTES))
        };
        let (file, _, body) =
            read_item(&mut reader, index, |string| body(string, profile, &retain))?;
        if let Some(body) = body.filter(|_| file.availability == PatchAvailability::Complete) {
            if primary && preferred.is_none_or(|required| required == file.id) {
                selected = Some((file.clone(), body));
            } else if !primary
                && let Body::Memory(bytes) = &body
                && bytes.capacity() <= remaining
            {
                remaining -= bytes.capacity();
                small.push((files.len(), Arc::new(body)));
            }
        }
        files.push(file);
    }
    reader.end_array().map_err(invalid)?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    Ok(DiffPage {
        files,
        selected,
        small,
    })
}

pub(super) fn find_file(
    wire: ResponseReader,
    first: u32,
    limit: u16,
    digest: &str,
) -> Result<Option<String>, AppError> {
    let mut reader = JsonStreamReader::new(wire);
    reader.begin_array().map_err(invalid)?;
    let mut ordinal = first;
    let mut found = None;
    while reader.has_next().map_err(invalid)? {
        if ordinal - first >= u32::from(limit) {
            return Err(AppError::Unavailable);
        }
        let mut old = None;
        let mut new = None;
        reader.begin_object().map_err(invalid)?;
        while reader.has_next().map_err(invalid)? {
            match reader.next_name().map_err(invalid)? {
                "old_path" if old.is_none() => old = Some(path(&mut reader)?),
                "new_path" if new.is_none() => new = Some(path(&mut reader)?),
                "old_path" | "new_path" => return Err(AppError::Unavailable),
                _ => reader.skip_value().map_err(invalid)?,
            }
        }
        reader.end_object().map_err(invalid)?;
        let id = file_lookup_key(
            ordinal,
            &old.ok_or(AppError::Unavailable)?,
            &new.ok_or(AppError::Unavailable)?,
        );
        if matches_digest(&id, digest) {
            if found.is_some() {
                return Err(AppError::Unavailable);
            }
            found = Some(id);
        }
        ordinal += 1;
    }
    reader.end_array().map_err(invalid)?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    Ok(found)
}

fn read_item<T>(
    reader: &mut JsonStreamReader<ResponseReader>,
    ordinal: u32,
    scan: impl FnOnce(&mut dyn Read) -> Result<(PatchMetrics, T, bool), AppError>,
) -> Result<(PatchFile, PatchMetrics, T), AppError> {
    let mut info = Diff::default();
    let mut payload = None;
    let mut scan = Some(scan);
    reader.begin_object().map_err(invalid)?;
    while reader.has_next().map_err(invalid)? {
        match reader.next_name().map_err(invalid)? {
            "old_path" if info.old_path.is_none() => info.old_path = Some(path(reader)?),
            "new_path" if info.new_path.is_none() => info.new_path = Some(path(reader)?),
            "renamed_file" if info.renamed.is_none() => {
                info.renamed = Some(reader.next_bool().map_err(invalid)?)
            }
            "new_file" if info.created.is_none() => {
                info.created = Some(reader.next_bool().map_err(invalid)?)
            }
            "deleted_file" if info.deleted.is_none() => {
                info.deleted = Some(reader.next_bool().map_err(invalid)?)
            }
            "collapsed" => info.collapsed = reader.next_bool().map_err(invalid)?,
            "too_large" => info.too_large = reader.next_bool().map_err(invalid)?,
            "binary" => info.binary = reader.next_bool().map_err(invalid)?,
            "diff" if payload.is_none() => {
                let mut string = reader.next_string_reader().map_err(invalid)?;
                payload = Some(scan.take().ok_or(AppError::Unavailable)?(&mut string)?);
            }
            "old_path" | "new_path" | "renamed_file" | "new_file" | "deleted_file" | "diff" => {
                return Err(AppError::Unavailable);
            }
            _ => reader.skip_value().map_err(invalid)?,
        }
    }
    reader.end_object().map_err(invalid)?;
    let old_path = info.old_path.ok_or(AppError::Unavailable)?;
    let new_path = info.new_path.ok_or(AppError::Unavailable)?;
    let (metrics, data, binary) = payload.ok_or(AppError::Unavailable)?;
    let availability = if info.binary || binary {
        PatchAvailability::Binary
    } else if info.too_large {
        PatchAvailability::ProviderLimited
    } else if info.collapsed {
        PatchAvailability::Incomplete
    } else {
        PatchAvailability::Complete
    };
    let complete = availability == PatchAvailability::Complete;
    Ok((
        PatchFile {
            id: file_lookup_key(ordinal, &old_path, &new_path),
            old_path,
            new_path,
            renamed: info.renamed.ok_or(AppError::Unavailable)?,
            created: info.created.ok_or(AppError::Unavailable)?,
            deleted: info.deleted.ok_or(AppError::Unavailable)?,
            availability,
            metrics: complete.then(|| metrics.clone()),
            preview: None,
        },
        metrics,
        data,
    ))
}

fn path(reader: &mut JsonStreamReader<ResponseReader>) -> Result<String, AppError> {
    let mut raw = Vec::new();
    reader
        .next_string_reader()
        .map_err(invalid)?
        .take(4097)
        .read_to_end(&mut raw)
        .map_err(invalid)?;
    let text = String::from_utf8(raw).map_err(invalid)?;
    if !safe_path(&text) {
        return Err(AppError::Unavailable);
    }
    Ok(text)
}

struct BodyScan {
    length: usize,
    lines: usize,
    max_line: usize,
    line_length: usize,
    was_cr: bool,
    digest: Sha256,
    stats: DiffStats,
}

impl BodyScan {
    fn new() -> Self {
        Self {
            length: 0,
            lines: 0,
            max_line: 0,
            line_length: 0,
            was_cr: false,
            digest: Sha256::new(),
            stats: DiffStats::new(),
        }
    }

    fn inspect(&mut self, chunk: &[u8]) {
        self.length += chunk.len();
        self.digest.update(chunk);
        for line in chunk.split_inclusive(|byte| *byte == b'\n') {
            let bytes = line.strip_suffix(b"\n").unwrap_or(line);
            self.stats.part(bytes, self.line_length == 0);
            self.line_length += bytes.len();
            if let Some(last) = bytes.last() {
                self.was_cr = *last == b'\r';
            }
            if bytes.len() != line.len() {
                self.lines += 1;
                self.max_line = self
                    .max_line
                    .max(self.line_length - usize::from(self.was_cr));
                self.stats.end_line();
                self.line_length = 0;
                self.was_cr = false;
            }
        }
    }

    fn finish(mut self) -> PatchMetrics {
        if self.line_length != 0 {
            self.stats.end_line();
        }
        PatchMetrics {
            byte_length: self.length,
            line_count: self.lines + usize::from(self.line_length != 0),
            max_line_bytes: self.max_line.max(self.line_length),
            line_changes: self.stats.finish(self.length == 0),
            etag: format!("\"{}\"", hex(&self.digest.finalize())),
        }
    }
}

fn body(
    mut reader: impl Read,
    profile: &Path,
    retain: &Retain<'_>,
) -> Result<(PatchMetrics, Option<Body>, bool), AppError> {
    let mut scan = BodyScan::new();
    // Read loop below uses fixed-sized blocks even for a multi-megabyte JSON string.
    let mut buffer = [0; 64 * 1024];
    let mut prefix = Vec::new();
    let mut keep = !matches!(
        retain,
        Retain::Small(0)
            | Retain::Selected(
                Selection {
                    policy: BodyPolicy::ReadAhead,
                    ..
                },
                _,
            )
    );
    let mut file = None::<ScratchFile>;
    let mut capture = match retain {
        Retain::Selected(selection, _) => Capture::new(selection.read, selection.policy),
        Retain::Small(_) => Capture::None,
    };
    let mut identification = Vec::new();
    loop {
        let length = reader.read(&mut buffer).map_err(invalid)?;
        if length == 0 {
            break;
        }
        let chunk = &buffer[..length];
        if identification.len() < 16 {
            let copy = (16 - identification.len()).min(chunk.len());
            identification.extend_from_slice(&chunk[..copy]);
        }
        capture.inspect(scan.length, chunk);
        scan.inspect(chunk);
        match retain {
            Retain::Small(limit) if scan.length > *limit => keep = false,
            Retain::Selected(selection, ordinal)
                if keep && file.is_none() && scan.length > MEMORY_LIMIT =>
            {
                if matches!(selection.policy, BodyPolicy::Lease) {
                    file = ScratchFile::create(
                        profile,
                        ScratchKey {
                            source: selection.source.clone(),
                            revision: selection.revision.clone(),
                            ordinal: *ordinal,
                        },
                    )
                    .and_then(|mut scratch| scratch.file.write_all(&prefix).ok().map(|_| scratch));
                }
                keep = file.is_some();
                prefix = Vec::new();
            }
            _ => {}
        }
        if keep {
            if let Some(file) = &mut file {
                keep = file.file.write_all(chunk).is_ok();
            } else {
                let limit = match retain {
                    Retain::Selected(_, _) => MEMORY_LIMIT,
                    Retain::Small(limit) => *limit,
                };
                if prefix.is_empty() {
                    prefix.reserve_exact(chunk.len());
                } else if scan.length > prefix.capacity() {
                    // Geometric growth without crossing the policy boundary.
                    let capacity = (prefix.capacity() * 2).max(scan.length).min(limit);
                    prefix.reserve_exact(capacity - prefix.len());
                }
                prefix.extend_from_slice(chunk);
            }
        }
        if !keep {
            file = None;
            prefix = Vec::new();
        }
    }
    let binary = binary_prefix(&identification);
    let data = if keep {
        Some(file.map_or(Body::Memory(prefix), |file| Body::File(file.into())))
    } else if matches!(retain, Retain::Selected(_, _)) {
        Some(capture.finish(scan.length))
    } else {
        None
    };
    Ok((scan.finish(), data, binary))
}

fn binary_prefix(prefix: &[u8]) -> bool {
    prefix.starts_with(b"Binary files ") || prefix.starts_with(b"GIT binary patch")
}

fn invalid<T>(_: T) -> AppError {
    AppError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::{BodyScan, DiffStats, Sha256};
    use sha2::Digest;

    #[test]
    fn counts_hunk_lines_across_chunks_and_withholds_partial_counts() {
        let scan = || BodyScan {
            length: 0,
            lines: 0,
            max_line: 0,
            line_length: 0,
            was_cr: false,
            digest: Sha256::new(),
            stats: DiffStats::new(),
        };
        let mut complete = scan();
        let patch = b"--- a/file\r\n+++ b/file\r\n@@ -1,3 +1,3 @@\r\n old\r\n--- old\r\n+++ new\r\n tail\r\n@@ -5 +5 @@\r\n-old\r\n+new\r\n\\ No newline at end of file";
        for chunk in patch.chunks(7) {
            complete.inspect(chunk);
        }
        let metrics = complete.finish();
        let counts = metrics.line_changes.unwrap();
        assert_eq!((counts.additions, counts.deletions), (2, 2));
        assert_eq!((metrics.line_count, metrics.byte_length), (11, patch.len()));

        let mut partial = scan();
        partial.inspect(b"@@ -0,0 +1,2 @@\n+only the first line");
        assert!(partial.finish().line_changes.is_none());
    }
}
