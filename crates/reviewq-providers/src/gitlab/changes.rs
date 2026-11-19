use std::fmt::Write as _;
use std::sync::Arc;

use reqwest::header::HeaderMap;
use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::changes::{
    ChangeSource, Discussion, PatchBytes, PatchFile, PatchRevision, SourcePage,
};
use reviewq_domain::identity::{GitSha, MrIdentity};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::GitLab;

mod discussions;
mod lease;
mod wire;

pub(super) use lease::Leases;
use lease::{IndexKey, Key};

const MAX_PATCH_FILES: u32 = 10_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct DiffRefs {
    base_sha: String,
    start_sha: String,
    head_sha: String,
}

impl ChangeSource for GitLab {
    fn files(
        &self,
        source: MrIdentity,
        revision: Option<String>,
        page: u32,
        limit: u16,
        selected_file: Option<String>,
    ) -> AppFuture<'_, SourcePage<PatchFile>> {
        Box::pin(async move {
            if !(1..=10).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            let selected = selected_file.as_deref();
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
                    let (scan, next) = self
                        .scan_diffs(&source, page, limit, selected, page == 1)
                        .await?;
                    if selected.is_some_and(|file| {
                        !scan.files.iter().any(|candidate| candidate.id == file)
                    }) {
                        return Err(AppError::NotFound);
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
                        next_page: next,
                    })
                })
                .await?;
            Ok(indexed.clone())
        })
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
        let refs = row.diff_refs.ok_or(AppError::Unavailable)?;
        let base = git_sha(&refs.base_sha)?;
        let start = git_sha(&refs.start_sha)?;
        let head = git_sha(&refs.head_sha)?;
        if head != git_sha(&row.sha)? {
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
        let first = page
            .checked_sub(1)
            .and_then(|page| page.checked_mul(u32::from(limit)))
            .and_then(|offset| offset.checked_add(1))
            .filter(|first| first <= &MAX_PATCH_FILES)
            .ok_or(AppError::PayloadTooLarge)?;
        let url = self.page_url(source, "diffs", page, limit)?;
        let (body, next) = {
            let response = self.http.get(url, self.headers.clone()).await?;
            let next = next_page(response.headers(), page)?;
            (wire::receive(response, &self.patches.profile).await?, next)
        };
        let profile = self.patches.profile.clone();
        let preferred = preferred.map(str::to_owned);
        let scanned = tokio::task::spawn_blocking(move || {
            wire::decode(body, &profile, first, limit, preferred.as_deref(), default)
        })
        .await
        .map_err(|_| AppError::Unavailable)??;
        Ok((scanned, next))
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

fn next_page(headers: &HeaderMap, page: u32) -> Result<Option<u32>, AppError> {
    let Some(value) = headers.get("x-next-page").filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let next: u32 = value
        .to_str()
        .ok()
        .and_then(|raw| raw.parse().ok())
        .ok_or(AppError::Unavailable)?;
    if page.checked_add(1) != Some(next) {
        return Err(AppError::Unavailable);
    }
    Ok(Some(next))
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
