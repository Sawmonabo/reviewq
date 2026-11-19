use std::fmt::Write as _;
use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::changes::{
    ApprovalView, ChangeSource, Discussion, PatchBytes, PatchFile, PatchFileSelection,
    PatchPathCursor, PatchPathPage, PatchRevision, SourcePage,
};
use reviewq_domain::identity::{GitSha, MrIdentity};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::{GitLab, next_page};
use crate::http::ResponseReader;

mod approvals;
mod discussions;
mod lease;
mod paths;
mod selection;
mod wire;

pub(super) use lease::Leases;
use lease::{IndexKey, Key};

const MAX_PATCH_FILES: u32 = 10_000;
const MAX_DIFF_RESPONSE: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct DiffRefs {
    base_sha: String,
    start_sha: String,
    head_sha: String,
}

impl ChangeSource for GitLab {
    fn revision(&self, source: MrIdentity) -> AppFuture<'_, PatchRevision> {
        Box::pin(async move { self.patch_revision(&source, None).await })
    }

    fn approvals(
        &self,
        source: MrIdentity,
        expected_head: Option<GitSha>,
        additional_reviewers: Vec<u64>,
    ) -> AppFuture<'_, ApprovalView> {
        Box::pin(self.approval_view(source, expected_head, additional_reviewers))
    }

    fn files(
        &self,
        source: MrIdentity,
        revision: Option<String>,
        page: u32,
        limit: u16,
        selected_file: Option<PatchFileSelection>,
    ) -> AppFuture<'_, SourcePage<PatchFile>> {
        Box::pin(async move {
            if !(1..=10).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            let selected = selected_file.as_ref().map(PatchFileSelection::key);
            let first_choice = selected
                .map(|id| ordinal(id).ok_or(AppError::InvalidRequest))
                .transpose()?;
            let page = if page == 1 {
                first_choice.map_or(page, |ordinal| 1 + (ordinal - 1) / u32::from(limit))
            } else {
                page
            };
            let current = self.patch_revision(&source, revision.as_deref()).await?;
            let slot = self
                .patches
                .index(IndexKey {
                    source: source.clone(),
                    revision: current.id.clone(),
                    offset: (page, limit),
                    preferred: selected_file.clone(),
                })
                .await?;
            let indexed = slot
                .get_or_try_init(|| async {
                    let (mut scan, mut next) = self
                        .scan_diffs(&source, page, limit, selected, page == 1 || limit == 1)
                        .await?;
                    let mut page = page;
                    if selected.is_some_and(|file| {
                        !scan.files.iter().any(|candidate| candidate.id == file)
                    }) {
                        let Some(PatchFileSelection::PathHint(hint)) = &selected_file else {
                            return Err(AppError::NotFound);
                        };
                        // Catalog order can differ from REST order. Only a REST file ID may
                        // address patch bytes or determine the selected file's position.
                        let (located_page, file) = self
                            .find_diff_file(&source, page, limit, hint, &scan.files)
                            .await?;
                        if located_page != page {
                            page = located_page;
                            (scan, next) = self
                                .scan_diffs(&source, page, limit, Some(&file), false)
                                .await?;
                            if !scan.files.iter().any(|candidate| candidate.id == file) {
                                return Err(AppError::NotFound);
                            }
                        }
                    }
                    self.patch_revision(&source, Some(&current.id)).await?;
                    if let Some((file, body)) = scan.selected
                        && let Ok(slot) = self
                            .patches
                            .slot(Key {
                                source,
                                revision: current.id.clone(),
                                file: file.id.clone(),
                            })
                            .await
                    {
                        let _ = slot
                            .get_or_init(|| async {
                                PatchBytes {
                                    revision: current.clone(),
                                    file,
                                    body: Arc::new(body),
                                }
                            })
                            .await;
                    }
                    Ok(SourcePage {
                        revision: current,
                        items: scan.files,
                        page,
                        next_page: next,
                    })
                })
                .await?;
            Ok(indexed.clone())
        })
    }

    fn selected(
        &self,
        source: MrIdentity,
        revision: String,
        ordinal: u32,
        hint: Option<String>,
    ) -> AppFuture<'_, reviewq_app::changes::SelectedPatch> {
        Box::pin(self.selected_patch(source, revision, ordinal, hint))
    }

    fn paths(
        &self,
        source: MrIdentity,
        project_path: String,
        revision: String,
        cursor: Option<PatchPathCursor>,
    ) -> AppFuture<'_, PatchPathPage> {
        Box::pin(self.path_page(source, project_path, revision, cursor))
    }

    fn patch(
        &self,
        source: MrIdentity,
        revision: String,
        file: String,
    ) -> AppFuture<'_, PatchBytes> {
        Box::pin(async move {
            let ordinal = ordinal(&file).ok_or(AppError::InvalidRequest)?;
            let current = self.patch_revision(&source, Some(&revision)).await?;
            let slot = self
                .patches
                .slot(Key {
                    source: source.clone(),
                    revision,
                    file: file.clone(),
                })
                .await?;
            let patch = slot
                .get_or_try_init(|| async {
                    let (scan, _) = self
                        .scan_diffs(&source, ordinal, 1, Some(&file), false)
                        .await?;
                    let (metadata, body) = match scan.selected {
                        Some(selected) => selected,
                        None if scan.files.iter().any(|row| row.id == file) => {
                            return Err(AppError::Unavailable);
                        }
                        None => return Err(AppError::NotFound),
                    };
                    self.patch_revision(&source, Some(&current.id)).await?;
                    Ok(PatchBytes {
                        revision: current,
                        file: metadata,
                        body: Arc::new(body),
                    })
                })
                .await?;
            Ok(patch.clone())
        })
    }

    fn discussions(
        &self,
        source: MrIdentity,
        revision: String,
        page: u32,
        limit: u16,
    ) -> AppFuture<'_, SourcePage<Discussion>> {
        Box::pin(self.discussion_page(source, revision, page, limit))
    }
}

impl GitLab {
    async fn patch_revision(
        &self,
        source: &MrIdentity,
        expected: Option<&str>,
    ) -> Result<PatchRevision, AppError> {
        if source.origin() != &self.identity {
            return Err(AppError::NotFound);
        }
        let row = self
            .merge_request(source.project_id(), source.iid())
            .await?;
        let source_head = row.head_sha()?;
        let refs = row.diff_refs.ok_or(AppError::Unavailable)?;
        revision_from_refs(source, refs, source_head, expected)
    }

    async fn read_diff_rows<T: DeserializeOwned>(
        &self,
        source: &MrIdentity,
        leaf: &str,
        page: u32,
        limit: u16,
        byte_limit: usize,
    ) -> Result<(Vec<T>, Option<u32>), AppError> {
        let url = self.page_url(source, leaf, page, limit)?;
        let (headers, bytes) = self.get(url, byte_limit).await?;
        let rows: Vec<T> = serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if rows.len() > usize::from(limit) {
            return Err(AppError::Unavailable);
        }
        Ok((rows, next_page(&headers, page)?))
    }

    async fn scan_diffs(
        &self,
        source: &MrIdentity,
        page: u32,
        limit: u16,
        preferred: Option<&str>,
        default: bool,
    ) -> Result<(wire::DiffPage, Option<u32>), AppError> {
        let first = first_ordinal(page, limit)?;
        let (body, next) = self.receive_diff_page(source, page, limit).await?;
        let profile = self.patches.profile.clone();
        let preferred = preferred.map(str::to_owned);
        let scanned = tokio::task::spawn_blocking(move || {
            wire::decode(body, &profile, first, limit, preferred.as_deref(), default)
        })
        .await
        .map_err(|_| AppError::Unavailable)??;
        Ok((scanned, next))
    }

    async fn receive_diff_page(
        &self,
        source: &MrIdentity,
        page: u32,
        limit: u16,
    ) -> Result<(ResponseReader, Option<u32>), AppError> {
        let url = self.page_url(source, "diffs", page, limit)?;
        let response = self
            .http
            .get(url, self.headers.clone(), self.lease.as_ref())
            .await?;
        let next = next_page(response.headers(), page)?;
        let body = response
            .bounded_reader(
                &self.patches.profile,
                lease::MEMORY_LIMIT,
                MAX_DIFF_RESPONSE,
            )
            .await?;
        if let Some(lease) = &self.lease {
            lease.check().await?;
        }
        Ok((body, next))
    }

    fn page_url(
        &self,
        source: &MrIdentity,
        leaf: &str,
        page: u32,
        limit: u16,
    ) -> Result<url::Url, AppError> {
        let mut url = self.url(&format!(
            "api/v4/projects/{}/merge_requests/{}/{leaf}",
            source.project_id(),
            source.iid()
        ))?;
        url.query_pairs_mut()
            .append_pair("page", &page.to_string())
            .append_pair("per_page", &limit.to_string());
        if leaf == "diffs" {
            url.query_pairs_mut().append_pair("unidiff", "true");
        }
        Ok(url)
    }
}

fn first_ordinal(page: u32, limit: u16) -> Result<u32, AppError> {
    page.checked_sub(1)
        .and_then(|page| page.checked_mul(u32::from(limit)))
        .and_then(|offset| offset.checked_add(1))
        .filter(|first| *first <= MAX_PATCH_FILES)
        .ok_or(AppError::PayloadTooLarge)
}

fn revision_from_refs(
    source: &MrIdentity,
    refs: DiffRefs,
    source_head: GitSha,
    expected: Option<&str>,
) -> Result<PatchRevision, AppError> {
    let base = git_sha(&refs.base_sha)?;
    let start = git_sha(&refs.start_sha)?;
    let head = git_sha(&refs.head_sha)?;
    if head != source_head {
        return Err(AppError::Unavailable);
    }
    let id = fingerprint(&[
        source.origin().as_str(),
        &source.project_id().to_string(),
        &source.iid().to_string(),
        base.as_str(),
        start.as_str(),
        head.as_str(),
    ]);
    if expected.is_some_and(|value| value != id) {
        return Err(AppError::StaleRevision);
    }
    Ok(PatchRevision {
        id,
        base,
        start,
        head,
    })
}

fn ordinal(file: &str) -> Option<u32> {
    let (raw, hash) = file.split_once('-')?;
    let index = raw.parse().ok()?;
    let canonical = hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    (canonical && (1..=MAX_PATCH_FILES).contains(&index)).then_some(index)
}

fn file_lookup_key(ordinal: u32, old_path: &str, new_path: &str) -> String {
    format!("{ordinal}-{}", fingerprint(&[old_path, new_path]))
}

fn safe_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control)
}

fn git_sha(raw: &str) -> Result<GitSha, AppError> {
    raw.parse().map_err(|_| AppError::Unavailable)
}

fn fingerprint(values: &[&str]) -> String {
    let mut sha = Sha256::new();
    for value in values {
        sha.update((value.len() as u64).to_be_bytes());
        sha.update(value.as_bytes());
    }
    hex(&sha.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut value, "{byte:02x}").expect("writing to a String");
    }
    value
}
