use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::changes::{
    PatchBytes, PatchContent, PatchFile, PatchRange, PatchRead, PatchRevision,
};
use reviewq_domain::identity::MrIdentity;
use tokio::sync::Mutex;

use super::lease::{Body, Key};
use super::wire::{BodyPolicy, Selection};
use super::{GitLab, ordinal};

const READ_AHEAD_BYTES: usize = 1024 * 1024;

struct RereadPatch {
    provider: GitLab,
    selection: Selection,
    ordinal: u32,
    length: usize,
    end: usize,
    etag: String,
    window: Mutex<Body>,
}

impl GitLab {
    pub(super) async fn read_patch(
        &self,
        source: MrIdentity,
        revision: String,
        file: String,
        read: PatchRead,
    ) -> Result<PatchBytes, AppError> {
        let ordinal = ordinal(&file).ok_or(AppError::InvalidRequest)?;
        let current = self.patch_revision(&source, Some(&revision)).await?;
        let key = Key {
            source: source.clone(),
            revision,
            file: file.clone(),
        };
        if let Some(patch) = self.patches.indexed_patch(&key).await {
            return Ok(patch);
        }
        let mut selection = Selection {
            source,
            revision: current.id.clone(),
            preferred: Some(file),
            default: false,
            read,
            policy: BodyPolicy::Lease,
        };
        let slot = match self.patches.slot(key).await {
            Ok(slot) => slot,
            Err(AppError::Busy) => {
                selection.policy = BodyPolicy::Uncached;
                let (file, body) = self.read_selected(ordinal, &selection).await?;
                return self.patch_bytes(current, file, body, selection, ordinal);
            }
            Err(error) => return Err(error),
        };
        let mut fallback = None;
        let result = slot
            .get_or_try_init(|| async {
                let (file, body) = self.read_selected(ordinal, &selection).await?;
                let is_window = matches!(body, Body::Window { .. });
                let patch = self.patch_bytes(current, file, body, selection, ordinal)?;
                if is_window {
                    // Only a complete retained body can populate the shared lease.
                    fallback = Some(patch);
                    Err(AppError::Unavailable)
                } else {
                    Ok(patch)
                }
            })
            .await;
        result.cloned().or_else(|failure| fallback.ok_or(failure))
    }

    async fn read_selected(
        &self,
        ordinal: u32,
        selection: &Selection,
    ) -> Result<(PatchFile, Body), AppError> {
        let (scan, _) = self.scan_diffs(ordinal, 1, selection).await?;
        self.patch_revision(&selection.source, Some(&selection.revision))
            .await?;
        if let Some(selected) = scan.selected {
            Ok(selected)
        } else if scan
            .files
            .iter()
            .any(|row| Some(row.id.as_str()) == selection.preferred.as_deref())
        {
            Err(AppError::Unavailable)
        } else {
            Err(AppError::NotFound)
        }
    }

    fn patch_bytes(
        &self,
        revision: PatchRevision,
        file: PatchFile,
        body: Body,
        mut selection: Selection,
        ordinal: u32,
    ) -> Result<PatchBytes, AppError> {
        let body: Arc<dyn PatchContent> = if matches!(body, Body::Window { .. }) {
            let metrics = file.metrics.as_ref().ok_or(AppError::Unavailable)?;
            let length = metrics.byte_length;
            let end = match selection.read {
                PatchRead::Full => length,
                PatchRead::Range(range) => range
                    .and_then(|range| range.resolve(length))
                    .map_or(0, |range| range.end),
            };
            selection.policy = BodyPolicy::ReadAhead;
            Arc::new(RereadPatch {
                provider: self.clone(),
                selection,
                ordinal,
                length,
                end,
                etag: metrics.etag.clone(),
                window: Mutex::new(body),
            })
        } else {
            Arc::new(body)
        };
        Ok(PatchBytes {
            revision,
            file,
            body,
        })
    }
}

impl PatchContent for RereadPatch {
    fn covers(&self, offset: usize, length: usize) -> bool {
        offset <= self.end && length <= self.end - offset
    }

    fn read(&self, offset: usize, length: usize) -> AppFuture<'_, Vec<u8>> {
        Box::pin(async move {
            if !self.covers(offset, length) || length > READ_AHEAD_BYTES {
                return Err(AppError::Internal);
            }
            let mut window = self.window.lock().await;
            if !window.covers(offset, length) {
                *window = Body::Window {
                    start: offset,
                    bytes: Vec::new(),
                };
                let last = (offset + READ_AHEAD_BYTES).min(self.end) - 1;
                let mut selection = self.selection.clone();
                selection.read = PatchRead::Range(Some(PatchRange::From {
                    start: offset,
                    last: Some(last),
                }));
                self.provider
                    .patch_revision(&selection.source, Some(&selection.revision))
                    .await?;
                let (file, fresh) = self
                    .provider
                    .read_selected(self.ordinal, &selection)
                    .await?;
                if !file.metrics.is_some_and(|metrics| {
                    metrics.etag == self.etag && metrics.byte_length == self.length
                }) {
                    return Err(AppError::PreconditionFailed);
                }
                *window = fresh;
            }
            window.read(offset, length).await
        })
    }
}
