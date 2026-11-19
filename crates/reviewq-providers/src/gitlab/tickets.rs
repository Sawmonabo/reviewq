use std::collections::{BTreeSet, HashSet};

use aho_corasick::AhoCorasick;
use reviewq_app::access::AppError;
use reviewq_app::sources::{
    HistoryCandidate, MAX_PUBLICATION_REVIEWS, MatchedReview, ProjectHistoryPage, ProjectHistoryRow,
};
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::TicketKey;

use super::{GitLab, lookup::key_boundaries};

const PAGE_LIMIT: u32 = 2_000;

pub(super) fn candidate_keys(title: &str, branch: &str) -> Result<Vec<TicketKey>, AppError> {
    let mut keys = BTreeSet::new();
    for field in [title, branch] {
        let bytes = field.as_bytes();
        for (dash, _) in field.match_indices('-') {
            let end = dash
                + 1
                + bytes[dash + 1..]
                    .iter()
                    .take_while(|byte| byte.is_ascii_digit())
                    .count();
            let prefix_length = bytes[..dash]
                .iter()
                .rev()
                .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
                .count();
            if end == dash + 1 || prefix_length == 0 || prefix_length > 100 {
                continue;
            }
            let prefix = dash - prefix_length;
            for start in std::iter::once(prefix).chain(
                field[prefix..dash]
                    .match_indices('_')
                    .map(|(i, _)| prefix + i + 1),
            ) {
                if start == dash || !key_boundaries(field, start, end) {
                    continue;
                }
                let key = field[start..end].to_ascii_uppercase();
                if crate::jira::valid_issue_key(&key) {
                    keys.insert(key);
                }
                if keys.len() > 16 {
                    return Err(AppError::PayloadTooLarge);
                }
            }
        }
    }
    keys.into_iter()
        .map(|key| key.parse().map_err(|_| AppError::InvalidRequest))
        .collect()
}

impl GitLab {
    pub(super) async fn match_open_reviews(
        &self,
        project: ConfiguredProject,
        keys: Vec<TicketKey>,
    ) -> Result<Vec<MatchedReview>, AppError> {
        if project.origin != self.identity || keys.is_empty() {
            return Err(AppError::InvalidRequest);
        }
        let matcher = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(keys.iter().map(TicketKey::as_str))
            .map_err(|_| AppError::PayloadTooLarge)?;
        let id = project.project_id;
        let metadata = self.project(id).await?;
        let mut seen = HashSet::new();
        let mut matches = Vec::new();
        for page in 1..=PAGE_LIMIT {
            let (rows, more) = self.merge_requests(id, page, "opened").await?;
            for row in rows {
                if !seen.insert(row.iid) {
                    return Err(AppError::Conflict);
                }
                if row.state != "opened" {
                    return Err(AppError::Conflict);
                }
                let mut matching = HashSet::new();
                for field in [&row.title, &row.source_branch] {
                    matching.extend(
                        matcher
                            .find_overlapping_iter(field)
                            .filter(|item| key_boundaries(field, item.start(), item.end()))
                            .map(|item| item.pattern().as_usize()),
                    );
                }
                if !matching.is_empty() {
                    if matches.len() == MAX_PUBLICATION_REVIEWS {
                        return Err(AppError::PayloadTooLarge);
                    }
                    matches.push(MatchedReview {
                        review: self.review(id, &metadata.path_with_namespace, row)?,
                        keys: matching
                            .into_iter()
                            .map(|index| keys[index].clone())
                            .collect(),
                    });
                }
            }
            if !more {
                return Ok(matches);
            }
        }
        Err(AppError::PayloadTooLarge)
    }

    pub(super) async fn history_page(
        &self,
        project: ConfiguredProject,
        page: u32,
        since: Option<i64>,
    ) -> Result<ProjectHistoryPage, AppError> {
        if project.origin != self.identity || !(1..=PAGE_LIMIT).contains(&page) {
            return Err(AppError::InvalidRequest);
        }
        let (rows, more) = self
            .merge_request_page(project.project_id, page, "all", true, since)
            .await?;
        let rows = rows
            .into_iter()
            .map(|row| {
                let iid = row.iid.try_into().map_err(|_| AppError::Unavailable)?;
                let closed = match row.state.as_str() {
                    "closed" | "merged" => true,
                    "opened" => false,
                    _ => return Err(AppError::Unavailable),
                };
                if row.title.len() > 4096 || row.source_branch.len() > 1024 {
                    return Err(AppError::PayloadTooLarge);
                }
                let updated_at_ms = row
                    .updated_at
                    .parse::<jiff::Timestamp>()
                    .map_err(|_| AppError::Unavailable)?
                    .as_millisecond();
                let keys = if closed {
                    candidate_keys(&row.title, &row.source_branch)?
                } else {
                    Vec::new()
                };
                Ok(ProjectHistoryRow {
                    iid,
                    closed,
                    updated_at_ms,
                    title: row.title,
                    branch: row.source_branch,
                    keys,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ProjectHistoryPage { rows, more })
    }

    pub(super) async fn history_candidates(
        &self,
        project: ConfiguredProject,
        candidates: Vec<HistoryCandidate>,
    ) -> Result<Vec<MatchedReview>, AppError> {
        if project.origin != self.identity || candidates.len() > MAX_PUBLICATION_REVIEWS {
            return Err(AppError::InvalidRequest);
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let id = project.project_id;
        let metadata = self.project(id).await?;
        let mut verified = Vec::new();
        for candidate in candidates {
            let row = match self.merge_request(id, candidate.iid).await {
                Ok(row) => row,
                Err(AppError::NotFound) => continue,
                Err(error) => return Err(error),
            };
            let mut exact = candidate_keys(&row.title, &row.source_branch)?;
            exact.retain(|key| candidate.keys.contains(key));
            if exact.is_empty() {
                continue;
            }
            let review = self.review(id, &metadata.path_with_namespace, row)?;
            if review.state == reviewq_app::queue::MergeRequestState::Open {
                return Err(AppError::Conflict);
            }
            verified.push(MatchedReview {
                review,
                keys: exact,
            });
        }
        Ok(verified)
    }
}
