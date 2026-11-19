use std::collections::HashSet;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::sources::{
    ScopedSprint, ScopedTicket, SprintPage, TicketDetail, TicketIdentity, TicketPage,
    TicketSourceReader,
};
use reviewq_domain::identity::TicketKey;
use reviewq_domain::settings::{BoardScope, BoardType};
use serde::Deserialize;
use serde_json::Value;

use super::{Jira, METADATA_LIMIT, numeric_id, safe_label, valid_estimate_field};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    issues: Vec<Issue>,
    next_page_token: Option<String>,
    is_last: Option<bool>,
}

#[derive(Deserialize)]
struct Issue {
    key: String,
    fields: Value,
}

impl TicketSourceReader for Jira {
    fn issue_identity(&self, key: TicketKey) -> AppFuture<'_, Option<TicketIdentity>> {
        Box::pin(async move {
            match self.ticket_identity(key).await {
                Ok(ticket) => Ok(Some(ticket)),
                Err(AppError::NotFound) => Ok(None),
                Err(error) => Err(error),
            }
        })
    }

    fn issue_detail(
        &self,
        key: TicketKey,
        estimate_fields: Vec<String>,
        offset: u32,
        limit: u16,
    ) -> AppFuture<'_, TicketDetail> {
        Box::pin(self.ticket_detail(key, estimate_fields, offset, limit))
    }

    fn sprints(&self, board: u64, offset: u32, include_closed: bool) -> AppFuture<'_, SprintPage> {
        Box::pin(async move {
            let mut parameters = vec![
                ("startAt", offset.to_string()),
                ("maxResults", "100".into()),
            ];
            if !include_closed {
                parameters.push(("state", "active,future".into()));
            }
            let page = self
                .catalog_page(
                    &format!("rest/agile/1.0/board/{board}/sprint"),
                    "values",
                    &parameters,
                    offset,
                    Some(board),
                )
                .await?;
            let mut items = Vec::with_capacity(page.items.len());
            for item in page.items {
                if !matches!(item.state.as_deref(), Some("active" | "future"))
                    && !(include_closed && item.state.as_deref() == Some("closed"))
                {
                    return Err(AppError::Conflict);
                }
                items.push(ScopedSprint {
                    board,
                    id: item.id.parse().map_err(|_| AppError::Unavailable)?,
                    label: item.label,
                    active: item.state.as_deref() == Some("active"),
                });
            }
            Ok(SprintPage {
                items,
                next: page.next,
            })
        })
    }

    fn issues(
        &self,
        board: BoardScope,
        sprint: Option<u64>,
        continuation: Option<String>,
        limit: u16,
    ) -> AppFuture<'_, TicketPage> {
        Box::pin(async move { self.issue_page(&board, sprint, continuation, limit).await })
    }
}

impl Jira {
    pub(crate) async fn issue_page(
        &self,
        board: &BoardScope,
        sprint: Option<u64>,
        continuation: Option<String>,
        limit: u16,
    ) -> Result<TicketPage, AppError> {
        if !(1..=100).contains(&limit) {
            return Err(AppError::InvalidRequest);
        }
        let spec = &board.selection;
        let route = match sprint {
            Some(id)
                if board.board_type == BoardType::Scrum && (1..=i64::MAX as u64).contains(&id) =>
            {
                format!(
                    "rest/software/1.0/board/{}/sprint/{id}/issue",
                    spec.board_id
                )
            }
            Some(_) => return Err(AppError::InvalidRequest),
            None => format!("rest/software/1.0/board/{}/issue", spec.board_id),
        };
        let mut clauses = vec![format!("status IN ({})", join_ids(&spec.statuses)?)];
        if !spec.jira_projects.is_empty() {
            if spec.jira_projects.iter().any(|key| !valid_project(key)) {
                return Err(AppError::InvalidRequest);
            }
            let projects = spec
                .jira_projects
                .iter()
                .map(|value| format!("\"{value}\""))
                .collect::<Vec<_>>();
            clauses.push(format!("project IN ({})", projects.join(",")));
        }
        if let Some(filter) = spec.filter {
            if !(1..=i64::MAX as u64).contains(&filter) {
                return Err(AppError::InvalidRequest);
            }
            clauses.push(format!("filter = {filter}"));
        }
        let mut fields = "summary,project,status,assignee,priority".to_owned();
        if let Some(estimate) = &spec.estimate_field {
            if !valid_estimate_field(estimate) {
                return Err(AppError::InvalidRequest);
            }
            fields.push(',');
            fields.push_str(estimate);
        }
        let mut parameters = vec![
            ("jql", clauses.join(" AND ")),
            ("fields", fields),
            ("maxResults", limit.to_string()),
        ];
        if let Some(next) = &continuation {
            validate_token(next)?;
            parameters.push(("nextPageToken", next.clone()));
        }
        let result: Page = self.read(&route, &parameters, METADATA_LIMIT).await?;
        if result.issues.len() > usize::from(limit)
            || result.is_last == Some(false) && result.next_page_token.is_none()
            || result.is_last == Some(true) && result.next_page_token.is_some()
        {
            return Err(AppError::Unavailable);
        }
        if let Some(token) = &result.next_page_token {
            validate_token(token)?;
            if result.issues.is_empty() || continuation.as_ref() == Some(token) {
                return Err(AppError::Unavailable);
            }
        }
        let mut keys = HashSet::new();
        let mut items = Vec::with_capacity(result.issues.len());
        for issue in result.issues {
            let fields = issue.fields;
            let project = safe_label(
                fields["project"]["key"]
                    .as_str()
                    .ok_or(AppError::Unavailable)?,
                100,
            )?;
            let (prefix, number) = issue.key.rsplit_once('-').ok_or(AppError::Unavailable)?;
            if prefix != project
                || !valid_project(prefix)
                || number.is_empty()
                || number.len() > 19
                || number.starts_with('0')
                || !number.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(AppError::Conflict);
            }
            let key = issue.key.parse().map_err(|_| AppError::Unavailable)?;
            if !keys.insert(issue.key) {
                return Err(AppError::Unavailable);
            }
            let status_id = numeric_id(&fields["status"]["id"]).ok_or(AppError::Unavailable)?;
            if !spec.statuses.contains(&status_id)
                || (!spec.jira_projects.is_empty() && !spec.jira_projects.contains(&project))
            {
                return Err(AppError::Conflict);
            }
            let (estimate, estimate_warning) =
                match spec.estimate_field.as_ref().and_then(|key| fields.get(key)) {
                    None | Some(Value::Null) => (None, None),
                    Some(value) => match value
                        .as_f64()
                        .filter(|value| value.is_finite() && *value >= 0.0)
                    {
                        Some(value) => (Some(value), None),
                        None => (None, Some("jira_estimate_invalid".to_owned())),
                    },
                };
            items.push(ScopedTicket {
                key,
                project,
                status_id,
                estimate,
                estimate_warning,
                title: safe_label(
                    fields["summary"].as_str().ok_or(AppError::Unavailable)?,
                    4096,
                )?,
                status: safe_label(
                    fields["status"]["name"]
                        .as_str()
                        .ok_or(AppError::Unavailable)?,
                    512,
                )?,
                assignee: optional_label(&fields["assignee"], "displayName")?,
                priority: optional_label(&fields["priority"], "name")?,
            });
        }
        Ok(TicketPage {
            items,
            continuation: result.next_page_token,
        })
    }

    pub(crate) async fn verify_filter(&self, filter: u64) -> Result<(), AppError> {
        if !(1..=i64::MAX as u64).contains(&filter) {
            return Err(AppError::InvalidRequest);
        }
        let found: Value = self
            .read(&format!("rest/api/2/filter/{filter}"), &[], METADATA_LIMIT)
            .await?;
        if numeric_id(&found["id"]) != Some(filter) {
            return Err(AppError::Conflict);
        }
        Ok(())
    }
}

pub(crate) fn valid_project(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 100
        && key.as_bytes()[0].is_ascii_uppercase()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn join_ids(ids: &[u64]) -> Result<String, AppError> {
    if ids.is_empty() || ids.len() > 100 || ids.iter().any(|id| !(1..=i64::MAX as u64).contains(id))
    {
        return Err(AppError::InvalidRequest);
    }
    Ok(ids.iter().map(u64::to_string).collect::<Vec<_>>().join(","))
}

fn validate_token(token: &str) -> Result<(), AppError> {
    if token.is_empty() || token.len() > 4096 || token.chars().any(char::is_control) {
        return Err(AppError::Unavailable);
    }
    Ok(())
}

pub(super) fn optional_label(value: &Value, key: &str) -> Result<Option<String>, AppError> {
    if value.is_null() {
        return Ok(None);
    }
    value[key]
        .as_str()
        .ok_or(AppError::Unavailable)
        .and_then(|value| safe_label(value, 512))
        .map(Some)
}
