use std::fmt::Write as _;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::changes::{
    ChangeSource, Discussion, PatchAvailability, PatchBytes, PatchFile, PatchMetrics,
    PatchRevision, SourcePage,
};
use reviewq_domain::identity::{GitSha, MrIdentity};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::GitLab;

mod discussions;

const MAX_PATCH_QUERY_BYTES: usize = 8 * 1024 * 1024;
const MAX_PATCH_FILES: u32 = 10_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct DiffRefs {
    base_sha: String,
    start_sha: String,
    head_sha: String,
}

#[derive(Deserialize)]
struct Diff {
    old_path: String,
    new_path: String,
    diff: String,
    renamed_file: bool,
    new_file: bool,
    deleted_file: bool,
    #[serde(default)]
    collapsed: bool,
    #[serde(default)]
    too_large: bool,
    #[serde(default)]
    binary: bool,
}

impl ChangeSource for GitLab {
    fn files(
        &self,
        source: MrIdentity,
        revision: Option<String>,
        page: u32,
        limit: u16,
    ) -> AppFuture<'_, SourcePage<PatchFile>> {
        Box::pin(async move {
            if !(1..=10).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            let offset = page
                .checked_sub(1)
                .and_then(|n| n.checked_mul(u32::from(limit)))
                .filter(|offset| *offset < MAX_PATCH_FILES)
                .ok_or(AppError::PayloadTooLarge)?;
            let current = self.patch_revision(&source, revision.as_deref()).await?;
            let (rows, next) = self
                .read_diff_rows::<Diff>(&source, "diffs", page, limit, MAX_PATCH_QUERY_BYTES)
                .await?;
            let mut files = Vec::with_capacity(rows.len());
            for (index, diff) in rows.into_iter().enumerate() {
                files.push(diff.file(offset + index as u32 + 1)?);
            }
            self.patch_revision(&source, Some(&current.id)).await?;
            Ok(SourcePage {
                revision: current,
                items: files,
                next_page: next,
            })
        })
    }

    fn patch(
        &self,
        source: MrIdentity,
        revision: String,
        file: String,
    ) -> AppFuture<'_, PatchBytes> {
        Box::pin(async move {
            let ordinal = file
                .split_once('-')
                .and_then(|(n, _)| n.parse::<u32>().ok())
                .filter(|n| (1..=MAX_PATCH_FILES).contains(n))
                .ok_or(AppError::InvalidRequest)?;
            let current = self.patch_revision(&source, Some(&revision)).await?;
            let (mut rows, _) = self
                .read_diff_rows::<Diff>(&source, "diffs", ordinal, 1, MAX_PATCH_QUERY_BYTES)
                .await?;
            let diff = rows.pop().ok_or(AppError::NotFound)?;
            let metadata = diff.file(ordinal)?;
            if metadata.id != file {
                return Err(AppError::NotFound);
            }
            self.patch_revision(&source, Some(&current.id)).await?;
            let bytes = match metadata.availability {
                PatchAvailability::Complete => diff.diff.into_bytes(),
                _ => Vec::new(),
            };
            Ok(PatchBytes {
                revision: current,
                file: metadata,
                bytes,
            })
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
        let (headers, bytes) = self.get(url, byte_limit).await?;
        let rows: Vec<T> = serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if rows.len() > usize::from(limit) {
            return Err(AppError::Unavailable);
        }
        let next = match headers.get("x-next-page") {
            Some(value) if !value.is_empty() => {
                let next: u32 = value
                    .to_str()
                    .ok()
                    .and_then(|raw| raw.parse().ok())
                    .ok_or(AppError::Unavailable)?;
                if page.checked_add(1) != Some(next) {
                    return Err(AppError::Unavailable);
                }
                Some(next)
            }
            _ => None,
        };
        Ok((rows, next))
    }
}

impl Diff {
    fn file(&self, ordinal: u32) -> Result<PatchFile, AppError> {
        if !safe_path(&self.old_path) || !safe_path(&self.new_path) {
            return Err(AppError::Unavailable);
        }
        let availability = if self.binary
            || self.diff.starts_with("Binary files ")
            || self.diff.starts_with("GIT binary patch")
        {
            PatchAvailability::Binary
        } else if self.too_large {
            PatchAvailability::ProviderLimited
        } else if self.collapsed {
            PatchAvailability::Incomplete
        } else {
            PatchAvailability::Complete
        };
        Ok(PatchFile {
            id: format!(
                "{ordinal}-{}",
                fingerprint(&[&self.old_path, &self.new_path])
            ),
            old_path: self.old_path.clone(),
            new_path: self.new_path.clone(),
            renamed: self.renamed_file,
            created: self.new_file,
            deleted: self.deleted_file,
            availability,
            metrics: (availability == PatchAvailability::Complete)
                .then(|| metrics(self.diff.as_bytes())),
        })
    }
}

fn metrics(bytes: &[u8]) -> PatchMetrics {
    let mut count = 0;
    let mut maximum = 0;
    for physical in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line = physical
            .strip_suffix(b"\n")
            .map_or(physical, |line| line.strip_suffix(b"\r").unwrap_or(line));
        count += 1;
        maximum = maximum.max(line.len());
    }
    PatchMetrics {
        byte_length: bytes.len(),
        line_count: count,
        max_line_bytes: maximum,
        etag: format!("\"{}\"", hex(&Sha256::digest(bytes))),
    }
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
