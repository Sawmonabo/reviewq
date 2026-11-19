use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use super::lease::{Body, MEMORY_LIMIT};
use super::paths::matches_digest;
use super::{file_lookup_key, hex, safe_path};
use crate::http::{ResponseReader, private_file};
use reviewq_app::access::AppError;
use reviewq_app::changes::{PatchAvailability, PatchFile, PatchMetrics};
use sha2::{Digest, Sha256};
use struson::reader::{JsonReader, JsonStreamReader};

mod stats;
use stats::DiffStats;

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
}

pub(super) fn decode(
    wire: ResponseReader,
    profile: &Path,
    first: u32,
    limit: u16,
    preferred: Option<&str>,
    select_default: bool,
) -> Result<DiffPage, AppError> {
    let mut reader = JsonStreamReader::new(wire);
    reader.begin_array().map_err(invalid)?;
    let preferred_index = preferred.and_then(super::ordinal);
    let mut files = Vec::new();
    let mut selected = None;
    while reader.has_next().map_err(invalid)? {
        if files.len() >= usize::from(limit) {
            return Err(AppError::Unavailable);
        }
        let index = first + files.len() as u32;
        let retain =
            preferred_index.map_or(select_default && selected.is_none(), |value| value == index);
        let (file, body) = read_item(&mut reader, profile, index, retain)?;
        if file.availability == PatchAvailability::Complete
            && preferred.is_none_or(|required| required == file.id)
            && let Some(body) = body
        {
            selected = Some((file.clone(), body));
        }
        files.push(file);
    }
    reader.end_array().map_err(invalid)?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    Ok(DiffPage { files, selected })
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

fn read_item(
    reader: &mut JsonStreamReader<ResponseReader>,
    profile: &Path,
    ordinal: u32,
    retain: bool,
) -> Result<(PatchFile, Option<Body>), AppError> {
    let mut info = Diff::default();
    let mut payload = None;
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
                let string = reader.next_string_reader().map_err(invalid)?;
                payload = Some(body(string, profile, retain)?);
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
            metrics: complete.then_some(metrics),
        },
        if complete { data } else { None },
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
    retain: bool,
) -> Result<(PatchMetrics, Option<Body>, bool), AppError> {
    let mut scan = BodyScan {
        length: 0,
        lines: 0,
        max_line: 0,
        line_length: 0,
        was_cr: false,
        digest: Sha256::new(),
        stats: DiffStats::new(),
    };
    // Read loop below uses fixed-sized blocks even for a multi-megabyte JSON string.
    let mut buffer = [0; 64 * 1024];
    let mut prefix = Vec::new();
    let mut file: Option<File> = None;
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
        scan.inspect(chunk);
        if retain {
            if file.is_none() && scan.length > MEMORY_LIMIT {
                let mut body = private_file(profile).map_err(|_| AppError::Internal)?;
                body.write_all(&prefix).map_err(|_| AppError::Internal)?;
                prefix = Vec::new();
                file = Some(body);
            }
            if let Some(file) = &mut file {
                file.write_all(chunk).map_err(|_| AppError::Internal)?;
            } else {
                prefix.extend_from_slice(chunk);
            }
        }
    }
    let binary = identification.starts_with(b"Binary files ")
        || identification.starts_with(b"GIT binary patch");
    let data = retain.then(|| file.map_or(Body::Memory(prefix), |file| Body::File(file.into())));
    Ok((scan.finish(), data, binary))
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
