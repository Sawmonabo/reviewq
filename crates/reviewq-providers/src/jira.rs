//! Jira Cloud reads against a verified site or its fixed Atlassian gateway.

use std::collections::HashSet;
use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::header::{self, HeaderMap, HeaderValue};
use reviewq_app::access::{AppError, Service};
use reviewq_app::settings::{CatalogItem, CatalogKind, CatalogPage, CatalogQuery, SecretToken};
use reviewq_domain::settings::{Connection, Identity, JiraTokenType};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::http::SourceNetwork;
use crate::settings::{origin, safe_label};

mod archive;
mod issues;
mod tickets;
pub(crate) use issues::valid_project;
pub(crate) use tickets::valid_issue_key;

const METADATA_LIMIT: usize = 1024 * 1024;
const PAGE_LIMIT: usize = 100;
const MAX_CATALOG_OFFSET: u32 = 200_000;
const BOARDS_PATH: &str = "rest/agile/1.0/board";

pub(crate) struct Jira {
    network: Arc<SourceNetwork>,
    cloud_id: String,
    site: Url,
    api: Url,
    headers: HeaderMap,
    lease: Option<reviewq_app::settings::SourceLease>,
    ticket_cache: tickets::Cache,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tenant {
    cloud_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    account_id: String,
    display_name: String,
    active: Option<bool>,
}

impl Jira {
    pub(crate) async fn connect(
        network: Arc<SourceNetwork>,
        connection: &Connection,
        token: &SecretToken,
        expected_cloud: Option<&str>,
        fixture: bool,
    ) -> Result<Self, AppError> {
        let Connection::Jira { site, .. } = connection else {
            return Err(AppError::InvalidRequest);
        };
        let site = origin(site, fixture)?;
        let tenant = site
            .join("_edge/tenant_info")
            .map_err(|_| AppError::Internal)?;
        let (_, body) = network
            .bounded_response(Service::Jira, tenant, HeaderMap::new(), 16 * 1024, None)
            .await?;
        let tenant: Tenant = serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)?;
        let id = uuid::Uuid::parse_str(&tenant.cloud_id).map_err(|_| AppError::Unavailable)?;
        if id.is_nil() || id.hyphenated().to_string() != tenant.cloud_id {
            return Err(AppError::Unavailable);
        }
        if expected_cloud.is_some_and(|expected| expected != tenant.cloud_id) {
            return Err(AppError::Conflict);
        }
        Self::for_bound(network, connection, token, &tenant.cloud_id, fixture, None)
    }

    pub(crate) fn for_bound(
        network: Arc<SourceNetwork>,
        connection: &Connection,
        token: &SecretToken,
        cloud: &str,
        fixture: bool,
        lease: Option<reviewq_app::settings::SourceLease>,
    ) -> Result<Self, AppError> {
        let Connection::Jira {
            site,
            email,
            token_type,
        } = connection
        else {
            return Err(AppError::InvalidRequest);
        };
        let site = origin(site, fixture)?;
        let id = uuid::Uuid::parse_str(cloud).map_err(|_| AppError::InvalidRequest)?;
        let api = match token_type {
            JiraTokenType::Unscoped => site.clone(),
            JiraTokenType::Scoped if fixture => site
                .join(&format!("ex/jira/{id}/"))
                .map_err(|_| AppError::Internal)?,
            JiraTokenType::Scoped => {
                Url::parse(&format!("https://api.atlassian.com/ex/jira/{id}/"))
                    .map_err(|_| AppError::Internal)?
            }
        };
        let secret = STANDARD.encode(format!("{email}:{}", token.expose()));
        let mut auth = HeaderValue::from_str(&format!("Basic {secret}"))
            .map_err(|_| AppError::InvalidRequest)?;
        auth.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, auth);
        Ok(Self {
            network,
            cloud_id: cloud.to_owned(),
            site,
            api,
            headers,
            lease,
            ticket_cache: tickets::Cache::default(),
        })
    }

    pub(crate) fn cloud_id(&self) -> &str {
        &self.cloud_id
    }

    pub(crate) async fn identity(&self) -> Result<Identity, AppError> {
        let reply: Account = self.read("rest/api/2/myself", &[], 64 * 1024).await?;
        if reply.active == Some(false) {
            return Err(AppError::Forbidden);
        }
        Ok(Identity {
            id: safe_label(&reply.account_id, 512)?,
            label: safe_label(&reply.display_name, 512)?,
        })
    }

    pub(crate) async fn catalog(&self, query: CatalogQuery) -> Result<CatalogPage, AppError> {
        let searchable = query
            .text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if query.kind == CatalogKind::Boards && query.states.is_empty() {
            return self.board_catalog(searchable, query.offset).await;
        }
        if query.offset >= MAX_CATALOG_OFFSET || query.states.len() > 3 {
            return Err(AppError::InvalidRequest);
        }
        let mut params = vec![
            ("startAt", query.offset.to_string()),
            ("maxResults", PAGE_LIMIT.to_string()),
        ];
        if !matches!(query.kind, CatalogKind::Sprints(_)) && !query.states.is_empty() {
            return Err(AppError::InvalidRequest);
        }
        if let Some(text) = searchable {
            params.push(("name", text.to_owned()));
        }
        match query.kind {
            CatalogKind::Filters => {
                if let Some((name, _)) = params.last_mut().filter(|(name, _)| *name == "name") {
                    *name = "filterName";
                }
                self.catalog_page(
                    "rest/api/2/filter/search",
                    "values",
                    &params,
                    query.offset,
                    None,
                )
                .await
            }
            CatalogKind::Statuses => {
                let rows: Vec<Value> = self.read("rest/api/2/status", &[], METADATA_LIMIT).await?;
                let mut items = unique_items(rows.iter(), None)?;
                if let Some(text) = searchable {
                    let query = text.to_lowercase();
                    items.retain(|item| item.label.to_lowercase().contains(&query));
                }
                items.sort_unstable_by(|left, right| {
                    left.id
                        .len()
                        .cmp(&right.id.len())
                        .then_with(|| left.id.cmp(&right.id))
                });
                let start = query.offset as usize;
                let end = start.saturating_add(PAGE_LIMIT);
                let next = (end < items.len()).then_some(end as u32);
                Ok(CatalogPage {
                    items: items.into_iter().skip(start).take(PAGE_LIMIT).collect(),
                    next,
                })
            }
            CatalogKind::Board(board) if query.offset == 0 && searchable.is_none() => {
                Ok(CatalogPage {
                    items: vec![self.board(board).await?],
                    next: None,
                })
            }
            CatalogKind::Sprints(board) => {
                let kind = self.board_kind(board).await?;
                if kind.1 != "scrum" {
                    return Err(AppError::InvalidRequest);
                }
                if !query
                    .states
                    .iter()
                    .all(|state| matches!(state.as_str(), "active" | "future" | "closed"))
                    || searchable.is_some()
                {
                    return Err(AppError::InvalidRequest);
                }
                if !query.states.is_empty() {
                    params.push(("state", query.states.join(",")));
                }
                self.catalog_page(
                    &format!("rest/agile/1.0/board/{board}/sprint"),
                    "values",
                    &params,
                    query.offset,
                    Some(board),
                )
                .await
            }
            _ => Err(AppError::InvalidRequest),
        }
    }

    async fn board_catalog(
        &self,
        text: Option<&str>,
        offset: u32,
    ) -> Result<CatalogPage, AppError> {
        if let Some(number) = text.map(|text| text.strip_prefix('#').unwrap_or(text))
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
        {
            if offset != 0 {
                return Err(AppError::InvalidRequest);
            }
            let mut items = Vec::new();
            if let Some(id) = number
                .parse()
                .ok()
                .filter(|id| (1..=i64::MAX as u64).contains(id))
            {
                match self.board_record(id).await {
                    Ok(row) => items.push(item(&row, None)?),
                    Err(AppError::NotFound) => {}
                    Err(error) => return Err(error),
                }
            }
            return Ok(CatalogPage { items, next: None });
        }

        let name = text.map(|value| ("name", value));
        let key = text
            .map(str::to_ascii_uppercase)
            .filter(|key| valid_project(key));
        let Some(key) = key else {
            return self.board_list(name, offset).await;
        };
        // The name continuation starts after the project-key pages. Both offsets passed to
        // Jira stay within its ordinary catalog paging window.
        if let Some(offset) = offset.checked_sub(MAX_CATALOG_OFFSET) {
            let mut page = self.board_list(name, offset).await?;
            page.next = page.next.map(|next| MAX_CATALOG_OFFSET + next);
            return Ok(page);
        }

        let mut page = match self
            .board_list(Some(("projectKeyOrId", &key)), offset)
            .await
        {
            Err(AppError::NotFound) if offset == 0 => CatalogPage {
                items: Vec::new(),
                next: None,
            },
            result => result?,
        };
        if page.next.is_some() {
            return Ok(page);
        }
        if page.items.len() == PAGE_LIMIT {
            page.next = Some(MAX_CATALOG_OFFSET);
            return Ok(page);
        }
        let names = self.board_list(name, 0).await?;
        let mut seen: HashSet<_> = page.items.iter().map(|item| item.id.clone()).collect();
        let mut names_remaining = names.items.into_iter();
        let mut consumed = 0;
        while page.items.len() < PAGE_LIMIT {
            let Some(item) = names_remaining.next() else {
                break;
            };
            consumed += 1;
            if seen.insert(item.id.clone()) {
                page.items.push(item);
            }
        }
        let next = if names_remaining.len() > 0 {
            Some(consumed)
        } else {
            names.next
        };
        page.next = next.map(|next| MAX_CATALOG_OFFSET + next);
        Ok(page)
    }

    async fn board_list(
        &self,
        filter: Option<(&str, &str)>,
        offset: u32,
    ) -> Result<CatalogPage, AppError> {
        if offset >= MAX_CATALOG_OFFSET {
            return Err(AppError::InvalidRequest);
        }
        let mut params = vec![
            ("startAt", offset.to_string()),
            ("maxResults", PAGE_LIMIT.to_string()),
        ];
        if let Some((name, value)) = filter {
            params.push((name, value.to_owned()));
        }
        self.catalog_page(BOARDS_PATH, "values", &params, offset, None)
            .await
    }

    async fn board_record(&self, board: u64) -> Result<Value, AppError> {
        if !(1..=i64::MAX as u64).contains(&board) {
            return Err(AppError::InvalidRequest);
        }
        let row: Value = self
            .read(&format!("{BOARDS_PATH}/{board}"), &[], METADATA_LIMIT)
            .await?;
        if numeric_id(&row["id"]) != Some(board) {
            return Err(AppError::Conflict);
        }
        Ok(row)
    }

    async fn board_kind(&self, board: u64) -> Result<(CatalogItem, &'static str), AppError> {
        let base = format!("{BOARDS_PATH}/{board}");
        let row = self.board_record(board).await?;
        let record = item(&row, Some(board))?;
        let kind = match row.get("type").and_then(Value::as_str) {
            Some("scrum") => "scrum",
            Some("kanban") => "kanban",
            Some("simple") => {
                let reply: Value = self
                    .read(&format!("{base}/features"), &[], METADATA_LIMIT)
                    .await?;
                let mut features = reply
                    .get("features")
                    .and_then(Value::as_array)
                    .ok_or(AppError::Unavailable)?
                    .iter()
                    .filter(|record| record["feature"] == "SPRINTS");
                let first = features.next().ok_or(AppError::Unavailable)?;
                if features.next().is_some() {
                    return Err(AppError::Unavailable);
                }
                match first["state"].as_str() {
                    Some("ENABLED") => "scrum",
                    Some("DISABLED") => "kanban",
                    _ => return Err(AppError::Unavailable),
                }
            }
            _ => return Err(AppError::Unavailable),
        };
        Ok((record, kind))
    }

    pub(crate) async fn board(&self, board: u64) -> Result<CatalogItem, AppError> {
        let (mut result, kind) = self.board_kind(board).await?;
        let route = format!("rest/agile/1.0/board/{board}/configuration");
        let row: Value = self.read(&route, &[], METADATA_LIMIT).await?;
        if numeric_id(&row["id"]) != Some(board) {
            return Err(AppError::Conflict);
        }
        let columns = row["columnConfig"]["columns"]
            .as_array()
            .ok_or(AppError::Unavailable)?;
        let mut statuses = HashSet::new();
        for column in columns {
            for status in column["statuses"].as_array().ok_or(AppError::Unavailable)? {
                statuses.insert(
                    numeric_id(&status["id"])
                        .ok_or(AppError::Unavailable)?
                        .to_string(),
                );
            }
        }
        if statuses.len() > 1000 {
            return Err(AppError::PayloadTooLarge);
        }
        result.status_ids = statuses.into_iter().collect();
        result.status_ids.sort_unstable();
        result.board_type = Some(kind.to_owned());
        result.estimate_field = row["estimation"]["field"]["fieldId"]
            .as_str()
            .map(|field| {
                if valid_estimate_field(field) {
                    Ok(field.to_owned())
                } else {
                    Err(AppError::Unavailable)
                }
            })
            .transpose()?;
        Ok(result)
    }

    pub(crate) async fn status_ids(&self) -> Result<HashSet<String>, AppError> {
        let rows: Vec<Value> = self.read("rest/api/2/status", &[], METADATA_LIMIT).await?;
        Ok(unique_items(rows.iter(), None)?
            .into_iter()
            .map(|item| item.id)
            .collect())
    }

    async fn catalog_page(
        &self,
        path: &str,
        key: &str,
        parameters: &[(&str, String)],
        offset: u32,
        board: Option<u64>,
    ) -> Result<CatalogPage, AppError> {
        let page: Value = self.read(path, parameters, METADATA_LIMIT).await?;
        let rows = page[key].as_array().ok_or(AppError::Unavailable)?;
        if rows.len() > PAGE_LIMIT || page["startAt"].as_u64() != Some(u64::from(offset)) {
            return Err(AppError::Unavailable);
        }
        let items = unique_items(rows.iter(), board)?;
        let end = offset + items.len() as u32;
        let more = match (page["isLast"].as_bool(), page["total"].as_u64()) {
            (Some(last), Some(total)) if last == (u64::from(end) >= total) => !last,
            (Some(last), None) => !last,
            (None, Some(total)) => u64::from(end) < total,
            _ => return Err(AppError::Unavailable),
        };
        if more && (items.is_empty() || end >= MAX_CATALOG_OFFSET) {
            return Err(AppError::Unavailable);
        }
        Ok(CatalogPage {
            items,
            next: more.then_some(end),
        })
    }

    async fn read<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        parameters: &[(&str, String)],
        limit: usize,
    ) -> Result<T, AppError> {
        let mut url = self.api.join(path).map_err(|_| AppError::Internal)?;
        if url.origin() != self.api.origin() || !url.path().starts_with(self.api.path()) {
            return Err(AppError::Forbidden);
        }
        if !parameters.is_empty() {
            url.query_pairs_mut().extend_pairs(parameters);
        }
        let (_, body) = self
            .network
            .bounded_response(
                Service::Jira,
                url,
                self.headers.clone(),
                limit,
                self.lease.as_ref(),
            )
            .await?;
        serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)
    }
}

pub(crate) fn valid_estimate_field(field: &str) -> bool {
    field.strip_prefix("customfield_").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.len() <= 18 && suffix.bytes().all(|b| b.is_ascii_digit())
    })
}

fn numeric_id(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|id| (1..=i64::MAX as u64).contains(id))
}

fn unique_items<'a>(
    rows: impl Iterator<Item = &'a Value>,
    board: Option<u64>,
) -> Result<Vec<CatalogItem>, AppError> {
    let mut ids = HashSet::new();
    rows.map(|value| {
        let item = item(value, board)?;
        if !ids.insert(item.id.clone()) {
            return Err(AppError::Unavailable);
        }
        Ok(item)
    })
    .collect()
}

fn item(value: &Value, board: Option<u64>) -> Result<CatalogItem, AppError> {
    let id = numeric_id(&value["id"])
        .ok_or(AppError::Unavailable)?
        .to_string();
    let label = safe_label(value["name"].as_str().ok_or(AppError::Unavailable)?, 512)?;
    let state = value["state"]
        .as_str()
        .map(|value| safe_label(value, 64))
        .transpose()?;
    Ok(CatalogItem {
        id,
        label,
        path: value["location"]["projectKey"]
            .as_str()
            .filter(|key| valid_project(key))
            .map(str::to_owned),
        board_id: board,
        state,
        board_type: None,
        status_ids: Vec::new(),
        estimate_field: None,
        reviewer: None,
        outside_view: None,
    })
}
