use aho_corasick::AhoCorasick;
use reviewq_app::access::AppError;
use reviewq_app::changes::{
    PatchAvailability, PatchBytes, PatchFile, PatchMetrics, PatchSearchCursor, PatchSearchMatch,
    PatchSearchPage, PatchSearchRequest,
};
use reviewq_domain::identity::MrIdentity;

use super::lease::Key;
use super::{GitLab, MAX_PATCH_FILES, first_ordinal, ordinal, wire};

mod rows;
use rows::Matches;
pub(super) use rows::Rows;

const FILES_PER_PAGE: u16 = 10;
const MAX_MATCHES: usize = 24;
// Keep space for envelope, numeric fields and cursor separately.
const MAX_MATCH_JSON_BYTES: usize = 224 * 1024;

#[derive(Clone)]
pub(super) struct SearchPage {
    pattern: AhoCorasick,
    pattern_bytes: usize,
    resume_file: u32,
    resume_offset: usize,
    resume_id: Option<String>,
    resume_etag: Option<String>,
    required_file: Option<String>,
    skip_file: Option<u32>,
}

pub(super) struct FileSearch {
    pub file: PatchFile,
    pub metrics: PatchMetrics,
    pub ordinal: u32,
    pub matches: Matches,
}

#[derive(Default)]
pub(super) struct SearchBatch {
    pub files: u32,
    pub continuation: Option<SearchContinuation>,
    hits: Vec<PatchSearchMatch>,
    json_bytes: usize,
    checked_visible: bool,
    partial: bool,
}

pub(super) struct SearchContinuation {
    ordinal: u32,
    offset: usize,
    file_id: String,
    etag: String,
}

impl GitLab {
    pub(super) async fn search_patches(
        &self,
        source: MrIdentity,
        request: PatchSearchRequest,
    ) -> Result<PatchSearchPage, AppError> {
        let visible = request
            .visible_file
            .as_deref()
            .map(|id| ordinal(id).ok_or(AppError::InvalidRequest))
            .transpose()?;
        let mut position = request.cursor.unwrap_or(PatchSearchCursor {
            page: u32::from(visible.is_none()),
            ordinal: 0,
            offset: 0,
            file_id: None,
            etag: None,
            checked_visible: false,
            partial: false,
            narrow: false,
        });
        let revision = self
            .patch_revision(&source, Some(&request.revision))
            .await?;
        let search = SearchPage {
            pattern: AhoCorasick::new([request.query.as_bytes()])
                .map_err(|_| AppError::Internal)?,
            pattern_bytes: request.query.len(),
            resume_file: position.ordinal,
            resume_offset: position.offset,
            resume_id: position.file_id.take(),
            resume_etag: position.etag.take(),
            required_file: request.visible_file.filter(|_| position.page == 0),
            skip_file: visible.filter(|_| position.checked_visible && position.page != 0),
        };
        let visible_pass = position.page == 0;
        let first = if visible_pass {
            visible.ok_or(AppError::CursorExpired)?
        } else {
            first_ordinal(position.page, FILES_PER_PAGE)?
        };
        let mut ordinal = first;
        let (batch, mut next) = if visible_pass {
            let key = Key {
                source: source.clone(),
                revision: revision.id.clone(),
                file: search
                    .required_file
                    .clone()
                    .ok_or(AppError::CursorExpired)?,
            };
            let known = if let Some(patch) = self.patches.leased_patch(&key).await {
                Some(patch)
            } else {
                self.patches.indexed_patch(&key).await
            };
            if let Some(patch) = known {
                (
                    Some(SearchBatch::from_patch(&search, first, patch).await?),
                    None,
                )
            } else {
                self.search_diff_page(&source, &revision.id, first, 1, &search)
                    .await?
            }
        } else if position.narrow {
            ordinal = position.ordinal.max(first);
            self.search_diff_page(&source, &revision.id, ordinal, 1, &search)
                .await?
        } else {
            let (batch, next) = self
                .search_diff_page(
                    &source,
                    &revision.id,
                    position.page,
                    FILES_PER_PAGE,
                    &search,
                )
                .await?;
            if batch.is_none() {
                position.narrow = true;
                ordinal = position.ordinal.max(first);
                self.search_diff_page(&source, &revision.id, ordinal, 1, &search)
                    .await?
            } else {
                (batch, next)
            }
        };
        let (_, provider_count) = self.diff_state(&source, Some(&revision.id)).await?;
        let batch = match batch {
            Some(batch) => batch,
            None => {
                position.partial = true;
                // The provider's pagination headers survived the body cap.
                SearchBatch {
                    files: 1,
                    ..SearchBatch::default()
                }
            }
        };
        if position.offset != 0
            && (ordinal - 1 + batch.files < position.ordinal
                || (visible_pass && !batch.checked_visible))
        {
            return Err(AppError::StaleRevision);
        }
        position.partial |= batch.partial;
        position.checked_visible |= batch.checked_visible;
        let continuation = if let Some(more) = batch.continuation {
            position.ordinal = more.ordinal;
            position.offset = more.offset;
            position.file_id = Some(more.file_id);
            position.etag = Some(more.etag);
            Some(position.clone())
        } else if visible_pass {
            position.page = 1;
            position.ordinal = 0;
            position.offset = 0;
            Some(position.clone())
        } else {
            let seen = ordinal - 1 + batch.files;
            position.partial |= next.is_none() && provider_count.is_some_and(|total| total > seen);
            if position.narrow && next.is_some() {
                if seen >= MAX_PATCH_FILES {
                    position.partial = true;
                    next = None;
                } else if seen % u32::from(FILES_PER_PAGE) == 0 {
                    next = Some(position.page + 1);
                    position.narrow = false;
                } else {
                    position.ordinal = seen + 1;
                    position.offset = 0;
                    return Ok(PatchSearchPage {
                        revision,
                        items: batch.hits,
                        partial: position.partial,
                        next: Some(position),
                    });
                }
            }
            if let Some(next) = next {
                if first_ordinal(next, FILES_PER_PAGE).is_ok() {
                    position.page = next;
                    position.ordinal = 0;
                    position.offset = 0;
                    Some(position.clone())
                } else {
                    position.partial = true;
                    None
                }
            } else {
                None
            }
        };
        Ok(PatchSearchPage {
            revision,
            items: batch.hits,
            partial: position.partial,
            next: continuation,
        })
    }

    async fn search_diff_page(
        &self,
        source: &MrIdentity,
        revision: &str,
        page: u32,
        limit: u16,
        search: &SearchPage,
    ) -> Result<(Option<SearchBatch>, Option<u32>), AppError> {
        let first = first_ordinal(page, limit)?;
        let search = search.clone();
        let (batch, next) = self
            .decode_diff_page(source, revision, page, limit, move |body| {
                wire::find_matches(body, first, limit, search)
            })
            .await?;
        let batch = match batch {
            Ok(batch) => batch,
            Err(AppError::PayloadTooLarge) => return Ok((None, next)),
            Err(error) => return Err(error),
        };
        if next.is_some() && batch.files != u32::from(limit) {
            return Err(AppError::Unavailable);
        }
        Ok((Some(batch), next))
    }
}

impl SearchPage {
    pub(super) fn includes(&self, ordinal: u32) -> bool {
        ordinal >= self.resume_file && self.skip_file != Some(ordinal)
    }

    pub(super) fn rows(&self, ordinal: u32, limit: usize) -> Rows<'_> {
        let after = if ordinal == self.resume_file {
            self.resume_offset
        } else {
            0
        };
        Rows::new(&self.pattern, self.pattern_bytes, after, limit)
    }
}

impl SearchBatch {
    pub(super) fn remaining(&self) -> usize {
        MAX_MATCHES - self.hits.len()
    }

    async fn from_patch(
        search: &SearchPage,
        ordinal: u32,
        patch: PatchBytes,
    ) -> Result<Self, AppError> {
        let mut batch = Self {
            files: 1,
            ..Self::default()
        };
        let metrics = patch.file.metrics.clone().ok_or(AppError::Unavailable)?;
        let mut rows = search.rows(ordinal, batch.remaining());
        let mut offset = 0;
        while offset < metrics.byte_length {
            let length = (64 * 1024).min(metrics.byte_length - offset);
            let chunk = patch.body.read(offset, length).await?;
            rows.part(&chunk);
            offset += length;
        }
        batch.add(
            search,
            FileSearch {
                file: patch.file,
                metrics,
                ordinal,
                matches: rows.finish(),
            },
        )?;
        Ok(batch)
    }

    pub(super) fn add(&mut self, query: &SearchPage, result: FileSearch) -> Result<(), AppError> {
        let FileSearch {
            file,
            metrics,
            ordinal,
            matches,
        } = result;
        if query
            .required_file
            .as_ref()
            .is_some_and(|required| *required != file.id)
        {
            return Ok(());
        }
        self.checked_visible = query.required_file.is_some();
        if ordinal == query.resume_file
            && (query
                .resume_etag
                .as_ref()
                .is_some_and(|tag| *tag != metrics.etag)
                || query.resume_id.as_ref().is_some_and(|id| *id != file.id))
        {
            return Err(AppError::StaleRevision);
        }
        if file.availability == PatchAvailability::Binary {
            return Ok(());
        }
        if file.availability != PatchAvailability::Complete {
            self.partial = true;
            return Ok(());
        }
        self.partial |= metrics.line_changes.is_none();
        let mut next = matches.next_offset;
        let paths_bytes = 2 * (file.old_path.len() + file.new_path.len());
        for hit in matches.hits {
            let charge = 384 + paths_bytes + 6 * hit.snippet.len();
            if charge > MAX_MATCH_JSON_BYTES - self.json_bytes {
                next = Some(hit.start);
                break;
            }
            self.json_bytes += charge;
            self.hits.push(PatchSearchMatch {
                file_id: file.id.clone(),
                patch_etag: metrics.etag.clone(),
                old_path: file.old_path.clone(),
                new_path: file.new_path.clone(),
                side: hit.side,
                line: hit.line,
                hunk: hit.hunk,
                source_start: hit.start,
                source_end: hit.end,
                snippet: hit.snippet,
                highlight_start: hit.highlight_start,
                highlight_end: hit.highlight_end,
            });
        }
        if let Some(offset) = next {
            self.continuation = Some(SearchContinuation {
                ordinal,
                offset,
                file_id: file.id,
                etag: metrics.etag,
            });
        }
        Ok(())
    }
}
