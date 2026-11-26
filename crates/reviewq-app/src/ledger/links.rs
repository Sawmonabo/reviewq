//! The web addresses a composed report links to. Each is built with the `url` crate from the
//! GitLab and Jira origins ReviewQ stores, so every path segment is percent-encoded.

use reviewq_domain::identity::{GitSha, MergeRequestIid, TicketKey};
use url::Url;

use crate::access::AppError;

/// Most ticket links the report's first line holds.
pub const MAX_TICKET_LINKS: usize = 3;

/// The GitLab project, merge request and Jira site that a version's links point into.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportLinks {
    /// `{origin}/{project path}`.
    project: Url,
    /// `{project}/-/merge_requests/{iid}`.
    merge_request: Url,
    /// The Jira site, when one is configured.
    jira: Option<Url>,
}

impl ReportLinks {
    /// The links of merge request `iid` in `project_path` on the GitLab `origin`, and of
    /// tickets on `jira_origin` when one is stored. Fails with `Internal` when an origin is
    /// not a URL that can hold a path.
    pub fn new(
        origin: &str,
        project_path: &str,
        iid: MergeRequestIid,
        jira_origin: Option<&str>,
    ) -> Result<Self, AppError> {
        let project = appended(&base(origin)?, project_path.split('/'));
        let merge_request = appended(&project, ["-", "merge_requests", &iid.to_string()]);
        let jira = jira_origin.map(base).transpose()?;
        Ok(Self {
            project,
            merge_request,
            jira,
        })
    }

    pub(super) fn project(&self) -> &Url {
        &self.project
    }

    pub(super) fn merge_request(&self) -> &Url {
        &self.merge_request
    }

    /// The Jira site's length, or 0 without one.
    pub(super) fn jira_len(&self) -> usize {
        self.jira.as_ref().map_or(0, |site| site.as_str().len())
    }

    /// `{jira origin}/browse/{key}`, or `None` when no Jira origin is stored.
    pub(super) fn ticket(&self, key: &TicketKey) -> Option<Url> {
        self.jira
            .as_ref()
            .map(|site| appended(site, ["browse", key.as_str()]))
    }

    /// The file at `path` in commit `sha`, at `line` when given:
    /// `{project}/-/blob/{sha}/{path}#L{line}`.
    pub(super) fn blob(&self, sha: &GitSha, path: &str, line: Option<u32>) -> Url {
        let mut url = appended(
            &self.project,
            ["-", "blob", sha.as_str()]
                .into_iter()
                .chain(path.split('/')),
        );
        if let Some(line) = line {
            url.set_fragment(Some(&format!("L{line}")));
        }
        url
    }

    /// `{project}/-/commit/{sha}`.
    pub(super) fn commit(&self, sha: &GitSha) -> Url {
        appended(&self.project, ["-", "commit", sha.as_str()])
    }

    /// The merge request's note `note`, a GitLab note ID of ASCII digits:
    /// `{merge request}#note_{note}`.
    pub(super) fn note(&self, note: &str) -> Url {
        let mut url = self.merge_request.clone();
        url.set_fragment(Some(&format!("note_{note}")));
        url
    }
}

/// `origin` parsed, or `Internal` when it is not a URL that can hold a path.
fn base(origin: &str) -> Result<Url, AppError> {
    Url::parse(origin)
        .ok()
        .filter(|url| !url.cannot_be_a_base())
        .ok_or(AppError::Internal)
}

/// `url` with `segments` appended after its path, each percent-encoded, and no empty segment
/// left between them by a trailing `/`. `url` can always hold a path, because `base` refuses
/// any other, so the append always happens.
fn appended<I>(url: &Url, segments: I) -> Url
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut url = url.clone();
    if let Ok(mut path) = url.path_segments_mut() {
        path.pop_if_empty().extend(segments);
    }
    url
}
