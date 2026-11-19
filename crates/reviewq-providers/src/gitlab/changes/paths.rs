use reviewq_app::access::AppError;
use reviewq_app::changes::{PatchFile, PatchPath, PatchPathCursor, PatchPathPage};
use reviewq_domain::identity::{MergeRequestIid, MrIdentity};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::json;

use super::{
    DiffRefs, GitLab, MAX_PATCH_FILES, file_lookup_key, first_ordinal, git_sha, revision_from_refs,
    safe_path, wire,
};

const PATHS_PER_PAGE: u32 = 100;
const MAX_PATH_PAGE_BYTES: usize = 1024 * 1024;
const PATH_QUERY: &str = r#"
query ReviewQDiffPaths($project: ID!, $iid: String!, $count: Int!, $after: String) {
  project(fullPath: $project) {
    mergeRequest(iid: $iid) {
      iid
      projectId
      diffHeadSha
      diffRefs { base_sha: baseSha start_sha: startSha head_sha: headSha }
      paths: diffs(first: $count, after: $after) {
        nodes { oldPath newPath }
        pageInfo { hasNextPage endCursor }
        overflow
      }
    }
  }
}
"#;

#[derive(Deserialize)]
struct Reply {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<IgnoredAny>,
}

#[derive(Deserialize)]
struct Data {
    project: Option<Project>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    merge_request: Option<MergeRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergeRequest {
    iid: String,
    project_id: u64,
    diff_head_sha: Option<String>,
    diff_refs: Option<DiffRefs>,
    paths: Option<Connection>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    nodes: Vec<Paths>,
    page_info: PageInfo,
    overflow: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Paths {
    old_path: String,
    new_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

impl GitLab {
    pub(super) async fn find_diff_file(
        &self,
        source: &MrIdentity,
        guessed_page: u32,
        limit: u16,
        hint: &str,
        page_files: &[PatchFile],
    ) -> Result<(u32, String), AppError> {
        let (_, digest) = hint.split_once('-').ok_or(AppError::InvalidRequest)?;
        if let Some(file) = page_files
            .iter()
            .find(|file| matches_digest(&file.id, digest))
        {
            return Ok((guessed_page, file.id.clone()));
        }

        // A catalog hint may nominate the wrong REST ordinal. Search ten paths at
        // a time even when the selection asks for only one result.
        let search_limit = 10;
        let mut page = 1;
        while first_ordinal(page, search_limit).is_ok() {
            if limit == search_limit && page == guessed_page {
                page += 1;
                continue;
            }
            let first = first_ordinal(page, search_limit)?;
            let (body, next) = self.receive_diff_page(source, page, search_limit).await?;
            let digest = digest.to_owned();
            let found = tokio::task::spawn_blocking(move || {
                wire::find_file(body, first, search_limit, &digest)
            })
            .await
            .map_err(|_| AppError::Unavailable)??;
            if let Some(id) = found {
                let ordinal = super::ordinal(&id).ok_or(AppError::Unavailable)?;
                return Ok((1 + (ordinal - 1) / u32::from(limit), id));
            }
            let Some(next) = next else {
                break;
            };
            page = next;
        }
        Err(AppError::NotFound)
    }

    pub(super) async fn path_page(
        &self,
        source: MrIdentity,
        project_path: String,
        revision: String,
        cursor: Option<PatchPathCursor>,
    ) -> Result<PatchPathPage, AppError> {
        if source.origin() != &self.identity || !safe_path(&project_path) {
            return Err(AppError::NotFound);
        }
        let (page, after, previous_partial) = cursor.as_ref().map_or((1, None, false), |cursor| {
            (cursor.page, Some(cursor.after.as_str()), cursor.partial)
        });
        if !(1..=MAX_PATCH_FILES / PATHS_PER_PAGE).contains(&page)
            || after.is_some_and(|value| !valid_provider_cursor(value))
        {
            return Err(AppError::CursorExpired);
        }
        let variables = json!({
            "project": project_path,
            "iid": source.iid().to_string(),
            "count": PATHS_PER_PAGE,
            "after": after,
        });
        let mut url = self.url("api/graphql")?;
        url.query_pairs_mut()
            .append_pair("query", PATH_QUERY)
            .append_pair("variables", &variables.to_string());
        let (_, bytes) = self.get(url, MAX_PATH_PAGE_BYTES).await?;
        let reply: Reply = serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if !reply.errors.is_empty() {
            return Err(AppError::Unavailable);
        }
        let data = reply.data.ok_or(AppError::Unavailable)?;
        let mr = data
            .project
            .and_then(|project| project.merge_request)
            .ok_or(AppError::NotFound)?;
        if mr.iid.parse::<MergeRequestIid>().ok() != Some(source.iid())
            || mr.project_id != source.project_id().get()
        {
            return Err(AppError::NotFound);
        }
        let head = git_sha(mr.diff_head_sha.as_deref().ok_or(AppError::Unavailable)?)?;
        let refs = mr.diff_refs.ok_or(AppError::Unavailable)?;
        let revision = revision_from_refs(&source, refs, head, Some(&revision))?;
        let paths = mr.paths.ok_or(AppError::Unavailable)?;
        if paths.nodes.len() > PATHS_PER_PAGE as usize {
            return Err(AppError::Unavailable);
        }
        let partial = previous_partial
            || paths.overflow
            || (paths.page_info.has_next_page
                && (page == MAX_PATCH_FILES / PATHS_PER_PAGE
                    || paths.nodes.len() < PATHS_PER_PAGE as usize));
        let next = if paths.page_info.has_next_page && page < MAX_PATCH_FILES / PATHS_PER_PAGE {
            let after = paths
                .page_info
                .end_cursor
                .filter(|value| valid_provider_cursor(value) && Some(value.as_str()) != after)
                .ok_or(AppError::Unavailable)?;
            Some(PatchPathCursor {
                page: page + 1,
                after,
                partial,
            })
        } else {
            None
        };
        let items = paths
            .nodes
            .into_iter()
            .enumerate()
            .map(|(offset, path)| {
                if !safe_path(&path.old_path) || !safe_path(&path.new_path) {
                    return Err(AppError::Unavailable);
                }
                // This key only nominates a REST page. The REST index checks the actual file identity.
                let ordinal = (page - 1) * PATHS_PER_PAGE + offset as u32 + 1;
                let lookup_key = file_lookup_key(ordinal, &path.old_path, &path.new_path);
                Ok(PatchPath {
                    old_path: path.old_path,
                    new_path: path.new_path,
                    lookup_key,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        Ok(PatchPathPage {
            revision,
            items,
            next,
            partial,
        })
    }
}

fn valid_provider_cursor(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(super) fn matches_digest(id: &str, digest: &str) -> bool {
    id.split_once('-').is_some_and(|(_, value)| value == digest)
}
