use std::collections::{BTreeSet, HashSet};

use reviewq_app::access::AppError;
use reviewq_app::queue::{ArchiveMembership, SprintPair};
use reviewq_app::sources::TicketSourceReader;
use reviewq_domain::identity::TicketKey;
use reviewq_domain::settings::{BoardScope, BoardType};
use serde::Deserialize;
use serde_json::Value;

use super::issues::validate_token;
use super::{Jira, METADATA_LIMIT, numeric_id, valid_issue_key, valid_project};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchivePage {
    issues: Vec<ArchiveIssue>,
    next_page_token: Option<String>,
    is_last: Option<bool>,
}

#[derive(Deserialize)]
struct ArchiveIssue {
    key: String,
    fields: Value,
}

impl Jira {
    pub(super) async fn archive_board(
        &self,
        board: BoardScope,
        keys: Vec<TicketKey>,
    ) -> Result<Vec<(TicketKey, ArchiveMembership)>, AppError> {
        if keys.len() > 8192 || keys.iter().any(|key| !valid_issue_key(key.as_str())) {
            return Err(AppError::InvalidRequest);
        }
        let id = board.selection.board_id;
        let (_, kind) = self.board_kind(id).await?;
        if (kind == "scrum") != (board.board_type == BoardType::Scrum) {
            return Err(AppError::Unavailable);
        }
        let known = if board.board_type == BoardType::Scrum {
            self.archive_sprints(id).await?
        } else {
            BTreeSet::new()
        };
        let mut seen = HashSet::new();
        let mut memberships = Vec::new();
        for batch in keys.chunks(100) {
            let items = self.archive_issues(&board, batch, false).await?;
            let mut local = Vec::new();
            let mut unknown = Vec::new();
            for item in items {
                let key = item
                    .key
                    .parse::<TicketKey>()
                    .map_err(|_| AppError::Unavailable)?;
                if !seen.insert(key.clone()) {
                    return Err(AppError::Conflict);
                }
                let mut membership = ArchiveMembership::default();
                if board.board_type == BoardType::Scrum {
                    let current = item.fields.get("sprint");
                    let closed = item.fields.get("closedSprints");
                    if let Some(sprint) = current.filter(|value| !value.is_null()) {
                        if let Some(id) = numeric_id(&sprint["id"]).filter(|id| known.contains(id))
                        {
                            membership.pairs.insert(SprintPair {
                                board: board.selection.board_id,
                                sprint: id,
                            });
                        }
                    }
                    if let Some(closed) = closed.filter(|value| !value.is_null()) {
                        let entries = closed.as_array().ok_or(AppError::Unavailable)?;
                        if entries.len() > 512 {
                            return Err(AppError::PayloadTooLarge);
                        }
                        for sprint in entries {
                            if let Some(id) =
                                numeric_id(&sprint["id"]).filter(|id| known.contains(id))
                            {
                                membership.pairs.insert(SprintPair {
                                    board: board.selection.board_id,
                                    sprint: id,
                                });
                            }
                        }
                    }
                    membership.complete = current.is_some() && closed.is_some();
                    if !membership.complete && membership.pairs.is_empty() {
                        unknown.push(key.clone());
                    }
                } else {
                    membership.complete = true;
                }
                local.push((key, membership));
            }
            if !unknown.is_empty() {
                let absent = self
                    .archive_issues(&board, &unknown, true)
                    .await?
                    .into_iter()
                    .map(|item| item.key)
                    .collect::<BTreeSet<_>>();
                for (key, membership) in &mut local {
                    if absent.contains(key.as_str()) {
                        membership.complete = true;
                    }
                }
            }
            memberships.extend(local);
        }
        Ok(memberships)
    }

    async fn archive_sprints(&self, board: u64) -> Result<BTreeSet<u64>, AppError> {
        let mut known = BTreeSet::new();
        let mut start = 0;
        for _ in 0..2000 {
            let response = self.sprints(board, start, true).await?;
            for sprint in response.items {
                if sprint.board != board || !known.insert(sprint.id) {
                    return Err(AppError::Conflict);
                }
                if known.len() > 8192 {
                    return Err(AppError::PayloadTooLarge);
                }
            }
            let Some(next) = response.next else {
                return Ok(known);
            };
            if next <= start {
                return Err(AppError::Conflict);
            }
            start = next;
        }
        Err(AppError::PayloadTooLarge)
    }

    async fn archive_issues(
        &self,
        board: &BoardScope,
        keys: &[TicketKey],
        without_sprint: bool,
    ) -> Result<Vec<ArchiveIssue>, AppError> {
        let spec = &board.selection;
        let clause = keys
            .iter()
            .map(|key| format!("\"{key}\""))
            .collect::<Vec<_>>()
            .join(",");
        let mut jql = format!("key IN ({clause})");
        if !spec.jira_projects.is_empty() {
            if spec.jira_projects.iter().any(|key| !valid_project(key)) {
                return Err(AppError::InvalidRequest);
            }
            let projects = spec
                .jira_projects
                .iter()
                .map(|key| format!("\"{key}\""))
                .collect::<Vec<_>>()
                .join(",");
            jql.push_str(&format!(" AND project IN ({projects})"));
        }
        if let Some(filter) = spec.filter {
            if !(1..=i64::MAX as u64).contains(&filter) {
                return Err(AppError::InvalidRequest);
            }
            jql.push_str(&format!(" AND filter = {filter}"));
        }
        if without_sprint {
            jql.push_str(" AND sprint IS EMPTY");
        }
        let route = format!("rest/software/1.0/board/{}/issue", spec.board_id);
        let mut output = Vec::new();
        let mut seen = HashSet::new();
        let mut continuation = None;
        for _ in 0..101 {
            let mut query = vec![
                ("jql", jql.clone()),
                ("fields", "project,sprint,closedSprints".into()),
                ("maxResults", "100".into()),
            ];
            if let Some(next) = continuation.take() {
                query.push(("nextPageToken", next));
            }
            let page: ArchivePage = self.read(&route, &query, METADATA_LIMIT).await?;
            if page.issues.len() + output.len() > keys.len()
                || (page.next_page_token.is_some() && page.issues.is_empty())
                || page
                    .is_last
                    .is_some_and(|last| last != page.next_page_token.is_none())
            {
                return Err(AppError::Unavailable);
            }
            for issue in page.issues {
                let (project, _) = issue.key.rsplit_once('-').ok_or(AppError::Unavailable)?;
                if issue.fields["project"]["key"].as_str() != Some(project)
                    || !keys.iter().any(|key| key.as_str() == issue.key)
                    || !seen.insert(issue.key.clone())
                {
                    return Err(AppError::Conflict);
                }
                output.push(issue);
            }
            let Some(next) = page.next_page_token else {
                return Ok(output);
            };
            validate_token(&next)?;
            if !seen.insert(format!("page:{next}")) {
                return Err(AppError::Conflict);
            }
            continuation = Some(next);
        }
        Err(AppError::PayloadTooLarge)
    }
}
