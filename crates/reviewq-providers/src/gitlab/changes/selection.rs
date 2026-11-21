use reviewq_app::access::AppError;
use reviewq_app::changes::{ChangeSource, PatchFileSelection, SelectedPatch};
use reviewq_domain::identity::MrIdentity;

use super::GitLab;
use super::lease::{INLINE_LIMIT, Key};

impl GitLab {
    pub(super) async fn selected_patch(
        &self,
        source: MrIdentity,
        revision: String,
        ordinal: u32,
        hint: Option<String>,
    ) -> Result<SelectedPatch, AppError> {
        // The REST index validates the exact file identity and brackets its scan
        // with ref checks. A matching lease owns the bytes from that same scan.
        let page = self
            .files(
                source.clone(),
                Some(revision.clone()),
                ordinal,
                1,
                hint.map(PatchFileSelection::PathHint),
                None,
            )
            .await?;
        let file = page.items.into_iter().next().ok_or(AppError::NotFound)?;
        let mut text = None;
        if let Some(metrics) = &file.metrics
            && metrics.byte_length <= INLINE_LIMIT
            && metrics.line_count <= 20_000
            && metrics.max_line_bytes <= 16 * 1024
            && let Some(patch) = self
                .patches
                .leased_patch(&Key {
                    source,
                    revision,
                    file: file.id.clone(),
                })
                .await
            && patch
                .file
                .metrics
                .as_ref()
                .is_some_and(|body| body.etag == metrics.etag)
        {
            let bytes = patch.body.read(0, metrics.byte_length).await?;
            text = Some(String::from_utf8(bytes).map_err(|_| AppError::Unavailable)?);
        }
        Ok(SelectedPatch {
            revision: page.revision,
            file,
            ordinal: page.page,
            next: page.next_page,
            text,
        })
    }
}
