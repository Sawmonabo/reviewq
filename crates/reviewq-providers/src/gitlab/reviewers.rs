//! Authenticated GitLab people search and saved reviewer identity reads.

use futures_util::{StreamExt, TryStreamExt, stream};
use reviewq_app::access::AppError;
use reviewq_app::settings::{CatalogItem, CatalogPage, CatalogQuery, CatalogReviewer};
use serde::Deserialize;

use super::{GitLab, METADATA_LIMIT, next_page};
use crate::settings::safe_label;

const PAGE_SIZE: usize = 20;
const MAX_PAGES: u32 = 100;

#[derive(Deserialize)]
struct User {
    id: u64,
    username: String,
    name: String,
    public_email: Option<String>,
}

impl GitLab {
    pub(crate) async fn reviewer_catalog(
        &self,
        query: CatalogQuery,
    ) -> Result<CatalogPage, AppError> {
        if !query.states.is_empty() || query.offset >= MAX_PAGES {
            return Err(AppError::InvalidRequest);
        }
        let Some(term) = query.text.as_deref().map(str::trim) else {
            let start = query.offset as usize * PAGE_SIZE;
            let count = query.saved_reviewer_ids.len();
            let ids = query.saved_reviewer_ids.into_iter().skip(start);
            let items = stream::iter(ids.take(PAGE_SIZE).map(|id| self.reviewer(id)))
                .buffered(4)
                .try_filter_map(|item| async move { Ok(item) })
                .try_collect::<Vec<_>>()
                .await?;
            let next = (start + PAGE_SIZE < count).then_some(query.offset + 1);
            return Ok(CatalogPage { items, next });
        };

        let number = term.strip_prefix('#').unwrap_or(term);
        if !number.is_empty() && number.bytes().all(|ch| ch.is_ascii_digit()) {
            if query.offset != 0 {
                return Err(AppError::InvalidRequest);
            }
            let id = number.parse::<u64>().ok().filter(|id| valid_id(*id));
            return Ok(CatalogPage {
                items: match id {
                    Some(id) => self.reviewer(id).await?.into_iter().collect(),
                    None => Vec::new(),
                },
                next: None,
            });
        }

        let handle = term.strip_prefix('@');
        let text = handle.unwrap_or(term);
        let public_email = handle.is_none() && text.contains('@');
        if text.chars().count() < 2
            || (public_email
                && !text.split_once('@').is_some_and(|(local, host)| {
                    !local.is_empty()
                        && !host.is_empty()
                        && !host.contains('@')
                        && !text.chars().any(char::is_whitespace)
                }))
        {
            return Ok(CatalogPage {
                items: Vec::new(),
                next: None,
            });
        }

        let page = query.offset + 1;
        let mut url = self.url("api/v4/users")?;
        url.query_pairs_mut()
            .append_pair(
                if public_email {
                    "public_email"
                } else {
                    "search"
                },
                text,
            )
            .append_pair("page", &page.to_string())
            .append_pair("per_page", &PAGE_SIZE.to_string());
        let (headers, body) = self.get(url, METADATA_LIMIT).await?;
        let people: Vec<User> = serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)?;
        if people.len() > PAGE_SIZE {
            return Err(AppError::PayloadTooLarge);
        }
        let next = next_page(&headers, page)?
            .filter(|_| !people.is_empty() && page < MAX_PAGES)
            .map(|_| page);
        let items = if public_email {
            stream::iter(people.into_iter().map(|user| self.reviewer(user.id)))
                .buffered(4)
                .try_filter_map(|item| async move {
                    Ok(item.filter(|item| {
                        item.reviewer
                            .as_ref()
                            .and_then(|reviewer| reviewer.public_email.as_deref())
                            .is_some_and(|email| email.eq_ignore_ascii_case(text))
                    }))
                })
                .try_collect::<Vec<_>>()
                .await?
        } else {
            let handle = handle.map(str::to_lowercase);
            people
                .into_iter()
                .filter(|user| {
                    handle
                        .as_ref()
                        .is_none_or(|handle| user.username.to_lowercase().contains(handle))
                })
                .map(User::into_item)
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(CatalogPage { items, next })
    }

    async fn reviewer(&self, id: u64) -> Result<Option<CatalogItem>, AppError> {
        if !valid_id(id) {
            return Err(AppError::Unavailable);
        }
        let (_, body) = match self
            .get(self.url(&format!("api/v4/users/{id}"))?, METADATA_LIMIT)
            .await
        {
            Ok(response) => response,
            Err(AppError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let person: User = serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)?;
        if person.id != id {
            return Err(AppError::Conflict);
        }
        person.into_item().map(Some)
    }
}

impl User {
    fn into_item(self) -> Result<CatalogItem, AppError> {
        if !valid_id(self.id) {
            return Err(AppError::Unavailable);
        }
        Ok(CatalogItem {
            id: self.id.to_string(),
            label: safe_label(&self.name, 512)?,
            path: None,
            board_id: None,
            state: None,
            board_type: None,
            status_ids: Vec::new(),
            estimate_field: None,
            reviewer: Some(CatalogReviewer {
                username: safe_label(&self.username, 256)?,
                public_email: self
                    .public_email
                    .and_then(|email| safe_label(&email, 320).ok()),
            }),
            outside_view: None,
        })
    }
}

fn valid_id(id: u64) -> bool {
    (1..=i64::MAX as u64).contains(&id)
}
