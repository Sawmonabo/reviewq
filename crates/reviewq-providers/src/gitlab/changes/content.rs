use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::changes::{
    PatchBytes, PatchContent, PatchFile, PatchRange, PatchRead, PatchRevision, PatchSourcePage,
    PatchSourceRequest, PatchSourceTarget, PatchSourceWindow,
};
use reviewq_domain::identity::MrIdentity;
use tokio::sync::Mutex;

use super::lease::{Body, Key, Storage};
use super::source::{SEEK_BEHIND_BYTES, SourceIndex, SourceRead};
use super::wire::{BodyPolicy, Selection};
use super::{GitLab, ordinal};

const READ_AHEAD_BYTES: usize = 1024 * 1024;

struct RereadPatch {
    provider: GitLab,
    selection: Selection,
    ordinal: u32,
    length: usize,
    etag: String,
    window: Mutex<Arc<Body>>,
    index: Arc<SourceIndex>,
}

struct RereadSource<'a> {
    patch: &'a RereadPatch,
    window: &'a mut Arc<Body>,
}

impl GitLab {
    pub(super) async fn read_source_window(
        &self,
        source: MrIdentity,
        request: PatchSourceRequest,
    ) -> Result<PatchSourcePage, AppError> {
        let _permit = self
            .patches
            .source_reads
            .try_acquire()
            .map_err(|_| AppError::Busy)?;
        let key = Key {
            source: source.clone(),
            revision: request.revision.clone(),
            file: request.file_id.clone(),
        };
        let patch = self
            .read_patch(
                source,
                request.revision,
                request.file_id,
                PatchRead::Source(request.target),
            )
            .await?;
        let metrics = patch.file.metrics.ok_or(AppError::Unavailable)?;
        if metrics.etag != request.etag {
            self.patches.discard_stale(&key, &request.etag).await;
            return Err(AppError::PreconditionFailed);
        }
        let window = patch.body.source_window(request.target).await;
        if matches!(window, Err(AppError::PreconditionFailed)) {
            self.patches.discard_stale(&key, &metrics.etag).await;
        }
        let window = window?;
        if window.total.byte != metrics.byte_length || window.total.row != metrics.line_count {
            return Err(AppError::Internal);
        }
        Ok(PatchSourcePage {
            revision: patch.revision,
            file_id: patch.file.id,
            etag: metrics.etag,
            window,
        })
    }

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
                return self.patch_bytes(current, file, Arc::new(body), selection);
            }
            Err(error) => return Err(error),
        };
        slot.get_or_try_init(|| async {
            let (file, body) = self.read_selected(ordinal, &selection).await?;
            self.patch_bytes(current, file, Arc::new(body), selection)
        })
        .await
        .cloned()
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

    pub(super) fn patch_bytes(
        &self,
        revision: PatchRevision,
        file: PatchFile,
        body: Arc<Body>,
        mut selection: Selection,
    ) -> Result<PatchBytes, AppError> {
        let body: Arc<dyn PatchContent> = if body.is_window() {
            let metrics = file.metrics.as_ref().ok_or(AppError::Unavailable)?;
            let ordinal = ordinal(&file.id).ok_or(AppError::Internal)?;
            selection.policy = BodyPolicy::ReadAhead;
            selection.preferred = Some(file.id.clone());
            selection.default = false;
            let index = body.index().ok_or(AppError::Internal)?.clone();
            let mut provider = self.clone();
            provider.patches = self.patches.refetch_only();
            Arc::new(RereadPatch {
                provider,
                selection,
                ordinal,
                length: metrics.byte_length,
                etag: metrics.etag.clone(),
                window: Mutex::new(body),
                index,
            })
        } else {
            body
        };
        Ok(PatchBytes {
            revision,
            file,
            body,
        })
    }
}

impl PatchContent for RereadPatch {
    fn retained(&self) -> bool {
        false
    }

    fn covers(&self, offset: usize, length: usize) -> bool {
        offset <= self.length && length <= self.length - offset
    }

    fn read(&self, offset: usize, length: usize) -> AppFuture<'_, Vec<u8>> {
        Box::pin(async move {
            let mut window = self.window.lock().await;
            self.read_cached(&mut window, offset, length).await
        })
    }

    fn source_window(&self, target: PatchSourceTarget) -> AppFuture<'_, PatchSourceWindow> {
        Box::pin(async move {
            let mut window = self.window.lock().await;
            let mut reader = RereadSource {
                patch: self,
                window: &mut window,
            };
            self.index.window(&mut reader, target).await
        })
    }
}

impl SourceRead for RereadSource<'_> {
    fn read(&mut self, offset: usize, length: usize) -> AppFuture<'_, Vec<u8>> {
        Box::pin(self.patch.read_cached(self.window, offset, length))
    }
}

impl RereadPatch {
    async fn read_cached(
        &self,
        window: &mut Arc<Body>,
        offset: usize,
        length: usize,
    ) -> Result<Vec<u8>, AppError> {
        if !self.covers(offset, length) || length > READ_AHEAD_BYTES {
            return Err(AppError::Internal);
        }
        if !window.covers(offset, length) {
            // The floor, checkpoint replay and packet all use this same verified
            // capture, even while another pane is requesting a distant packet.
            let overlap = SEEK_BEHIND_BYTES.min(READ_AHEAD_BYTES - length);
            let start = offset.saturating_sub(overlap);
            *window = Arc::new(Body::new(
                Storage::Window {
                    start,
                    bytes: Vec::new(),
                },
                None,
            ));
            let last = (start + READ_AHEAD_BYTES).min(self.length) - 1;
            let mut selection = self.selection.clone();
            selection.read = PatchRead::Range(Some(PatchRange::From {
                start,
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
            *window = Arc::new(fresh);
        }
        window.read(offset, length).await
    }
}
