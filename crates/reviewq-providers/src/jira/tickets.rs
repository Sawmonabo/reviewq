use std::collections::HashSet;

use reviewq_app::access::AppError;
use reviewq_app::sources::{
    CommentOrder, TicketComment, TicketCommentPage, TicketDetail, TicketIdentity, TicketOpening,
};
use reviewq_domain::identity::TicketKey;
use serde::Deserialize;
use serde_json::Value;

use super::issues::optional_label;
use super::{Jira, METADATA_LIMIT, numeric_id, safe_label, valid_estimate_field, valid_project};

mod cache;
pub(super) use cache::Cache;

const DESCRIPTION_LIMIT: usize = 256 * 1024;
const COMMENT_LIMIT: usize = 64 * 1024;
const COMMENTS_PAGE: u16 = 26;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comments {
    start_at: u32,
    total: u32,
    comments: Vec<Comment>,
}

#[derive(Deserialize)]
struct Comment {
    id: Value,
    author: Option<Value>,
    body: String,
    created: String,
    updated: Option<String>,
}

impl Jira {
    pub(super) async fn ticket_identity(&self, key: TicketKey) -> Result<TicketIdentity, AppError> {
        let (identity, _, _) = self.ticket(key, "summary,project".into()).await?;
        Ok(identity)
    }

    pub(super) async fn ticket_detail(
        &self,
        key: TicketKey,
        mut estimate_fields: Vec<String>,
        limit: u16,
        order: CommentOrder,
    ) -> Result<TicketOpening, AppError> {
        if !(1..=COMMENTS_PAGE).contains(&limit)
            || estimate_fields
                .iter()
                .any(|field| !valid_estimate_field(field))
        {
            return Err(AppError::InvalidRequest);
        }
        estimate_fields.sort_unstable();
        estimate_fields.dedup();
        let fetch = || self.fetch_ticket_detail(key.clone(), estimate_fields.clone(), limit, order);
        let result = if let Some(lease) = &self.lease {
            lease.check().await?;
            let result = self
                .ticket_cache
                .read(
                    key.clone(),
                    estimate_fields.clone(),
                    limit,
                    order,
                    fetch,
                    |issue| self.authorized_comments(key.clone(), issue, 0, limit, order),
                )
                .await;
            lease.check().await?;
            result
        } else {
            fetch().await
        };
        self.invalidate_on_denial(&key, result)
    }

    async fn fetch_ticket_detail(
        &self,
        key: TicketKey,
        estimate_fields: Vec<String>,
        limit: u16,
        order: CommentOrder,
    ) -> Result<TicketOpening, AppError> {
        let fields = std::iter::once(
            "summary,project,status,priority,assignee,reporter,issuetype,labels,components,description,updated",
        )
        .chain(estimate_fields.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(",");
        let (identity, id, fields) = self.ticket(key, fields).await?;
        let updated_at_ms = match &fields["updated"] {
            Value::Null => None,
            Value::String(updated) => Some(time(updated)?),
            _ => return Err(AppError::Unavailable),
        };
        let description = match &fields["description"] {
            Value::Null => None,
            Value::String(text) if text.len() <= DESCRIPTION_LIMIT => Some(text.clone()),
            Value::String(_) => return Err(AppError::PayloadTooLarge),
            _ => return Err(AppError::Unavailable),
        };
        let (estimate, warning) = estimate(&fields, &estimate_fields);
        let comments = self
            .ticket_comments(&identity.key, id, 0, limit, order)
            .await?;
        let detail = TicketDetail {
            identity,
            status: optional_label(&fields["status"], "name")?,
            priority: optional_label(&fields["priority"], "name")?,
            assignee: optional_label(&fields["assignee"], "displayName")?,
            assignee_id: account_id(&fields["assignee"])?,
            reporter: optional_label(&fields["reporter"], "displayName")?,
            reporter_id: account_id(&fields["reporter"])?,
            issue_type: optional_label(&fields["issuetype"], "name")?,
            labels: detail_names(&fields["labels"], None)?,
            components: detail_names(&fields["components"], Some("name"))?,
            description,
            estimate,
            updated_at_ms,
            warning,
        };
        Ok(TicketOpening { detail, comments })
    }

    pub(super) async fn authorized_comments(
        &self,
        key: TicketKey,
        expected_issue: u64,
        offset: u32,
        limit: u16,
        order: CommentOrder,
    ) -> Result<TicketCommentPage, AppError> {
        // Jira checks issue access on both reads and applies comment visibility on the second.
        let result = async {
            let (issue, _) = self.ticket_fields(&key, "project".into()).await?;
            if issue != expected_issue {
                return Err(AppError::CursorExpired);
            }
            self.ticket_comments(&key, issue, offset, limit, order)
                .await
        }
        .await;
        self.invalidate_on_denial(&key, result)
    }

    fn invalidate_on_denial<T>(
        &self,
        key: &TicketKey,
        result: Result<T, AppError>,
    ) -> Result<T, AppError> {
        if matches!(
            result,
            Err(AppError::Forbidden
                | AppError::NotFound
                | AppError::Conflict
                | AppError::CursorExpired)
        ) {
            self.ticket_cache.remove(key);
        }
        result
    }

    async fn ticket(
        &self,
        key: TicketKey,
        fields: String,
    ) -> Result<(TicketIdentity, u64, Value), AppError> {
        let (id, fields) = self.ticket_fields(&key, fields).await?;
        let title = safe_label(
            fields["summary"].as_str().ok_or(AppError::Unavailable)?,
            4096,
        )?;
        Ok((TicketIdentity { key, title }, id, fields))
    }

    async fn ticket_fields(
        &self,
        key: &TicketKey,
        fields: String,
    ) -> Result<(u64, Value), AppError> {
        if !valid_issue_key(key.as_str()) {
            return Err(AppError::InvalidRequest);
        }
        let mut item: Value = self
            .read(
                &format!("rest/api/2/issue/{key}"),
                &[("fields", fields)],
                METADATA_LIMIT,
            )
            .await?;
        let fields = item["fields"].take();
        let (project, _) = key.as_str().rsplit_once('-').ok_or(AppError::Internal)?;
        if item["key"].as_str() != Some(key.as_str())
            || fields["project"]["key"].as_str() != Some(project)
        {
            return Err(AppError::Conflict);
        }
        let id = numeric_id(&item["id"]).ok_or(AppError::Unavailable)?;
        Ok((id, fields))
    }

    async fn ticket_comments(
        &self,
        key: &TicketKey,
        issue: u64,
        offset: u32,
        limit: u16,
        order: CommentOrder,
    ) -> Result<TicketCommentPage, AppError> {
        if !(1..=COMMENTS_PAGE).contains(&limit) {
            return Err(AppError::InvalidRequest);
        }
        let ordering = match order {
            CommentOrder::Newest => "-created",
            CommentOrder::Oldest => "created",
        };
        let page: Comments = self
            .read(
                &format!("rest/api/2/issue/{issue}/comment"),
                &[
                    ("orderBy", ordering.into()),
                    ("startAt", offset.to_string()),
                    ("maxResults", limit.to_string()),
                ],
                2 * METADATA_LIMIT,
            )
            .await?;
        let end = offset
            .checked_add(page.comments.len() as u32)
            .ok_or(AppError::Unavailable)?;
        if page.start_at != offset
            || page.comments.len() > usize::from(limit)
            || end > page.total
            || (end < page.total && page.comments.is_empty())
        {
            return Err(AppError::CursorExpired);
        }
        let mut comments = Vec::with_capacity(page.comments.len());
        let mut ids = HashSet::with_capacity(page.comments.len());
        for item in page.comments {
            let id = numeric_id(&item.id).ok_or(AppError::Unavailable)?;
            let created_at_ms = time(&item.created)?;
            if !ids.insert(id)
                || comments
                    .last()
                    .is_some_and(|previous: &TicketComment| match order {
                        CommentOrder::Newest => previous.created_at_ms < created_at_ms,
                        CommentOrder::Oldest => previous.created_at_ms > created_at_ms,
                    })
            {
                return Err(AppError::Unavailable);
            }
            if item.body.len() > COMMENT_LIMIT {
                return Err(AppError::PayloadTooLarge);
            }
            comments.push(TicketComment {
                id,
                author_id: item.author.as_ref().map(account_id).transpose()?.flatten(),
                author: item
                    .author
                    .as_ref()
                    .map(|author| optional_label(author, "displayName"))
                    .transpose()?
                    .flatten(),
                body: item.body,
                created_at_ms,
                updated_at_ms: item.updated.as_deref().map(time).transpose()?,
            });
        }
        let url = self
            .site
            .join(&format!("browse/{key}"))
            .map_err(|_| AppError::Internal)?;
        Ok(TicketCommentPage {
            issue,
            url: url.into(),
            fetched_at_ms: jiff::Timestamp::now().as_millisecond(),
            items: comments,
            total: page.total,
            next_offset: (end < page.total).then_some(end),
        })
    }
}

pub(crate) fn valid_issue_key(key: &str) -> bool {
    let Some((prefix, number)) = key.rsplit_once('-') else {
        return false;
    };
    valid_project(prefix)
        && !number.is_empty()
        && number.len() <= 19
        && !number.starts_with('0')
        && number.bytes().all(|byte| byte.is_ascii_digit())
}

fn time(value: &str) -> Result<i64, AppError> {
    value
        .parse::<jiff::Timestamp>()
        .map(|time| time.as_millisecond())
        .map_err(|_| AppError::Unavailable)
}

fn account_id(person: &Value) -> Result<Option<String>, AppError> {
    person["accountId"]
        .as_str()
        .map(|id| safe_label(id, 512))
        .transpose()
}

fn detail_names(value: &Value, member: Option<&str>) -> Result<Vec<String>, AppError> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let entries = value.as_array().ok_or(AppError::Unavailable)?;
    if entries.len() > 256 {
        return Err(AppError::PayloadTooLarge);
    }
    entries
        .iter()
        .map(|item| {
            let value = member.map_or(item, |key| &item[key]);
            safe_label(value.as_str().ok_or(AppError::Unavailable)?, 512)
        })
        .collect()
}

fn estimate(fields: &Value, columns: &[String]) -> (Option<f64>, Option<&'static str>) {
    let mut estimate = None;
    for value in columns
        .iter()
        .filter_map(|column| fields.get(column))
        .filter(|value| !value.is_null())
    {
        let Some(points) = value
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0)
        else {
            return (None, Some("jira_estimate_invalid"));
        };
        if estimate.is_some_and(|previous| previous != points) {
            return (None, Some("jira_estimate_conflict"));
        }
        estimate = Some(points);
    }
    (estimate, None)
}
