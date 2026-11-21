use std::collections::HashSet;
use std::sync::Arc;

use reviewq_app::access::AppError;
use reviewq_app::changes::{
    DiscussionDraft, DiscussionDrafts, DiscussionScope, DiscussionTarget, NoteAddress,
    PatchRevision,
};
use reviewq_domain::identity::MrIdentity;
use reviewq_domain::settings::Identity;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    GitLab, MAX_DISCUSSION_BYTES, Position, UpstreamDiscussion, cache, content_fingerprint, project,
};

const MAX_DRAFTS: usize = 250;

#[derive(Deserialize, Serialize)]
struct Draft {
    id: u64,
    author_id: u64,
    merge_request_id: u64,
    discussion_id: Option<String>,
    note: String,
    position: Option<Value>,
}

impl GitLab {
    pub(in crate::gitlab::changes) async fn read_discussion_drafts(
        &self,
        source: MrIdentity,
        scope: DiscussionScope,
    ) -> Result<DiscussionDrafts, AppError> {
        let (revision, mr_id) = self.discussion_context(&source, &scope.revision).await?;
        let id = valid_id(mr_id).ok_or(AppError::Unavailable)?;
        let observation = self.discussions.observe(&source, mr_id, &scope)?;
        if let Some(cached) = observation.drafts.get() {
            if let Some(witness) = cached.items.first() {
                let wire: Draft = self
                    .read_note(&source, "draft_notes", Some(&witness.id.to_string()))
                    .await
                    .map_err(lost_observation)?;
                if draft(wire, id, witness.account, &witness.author, &revision)
                    .map_err(lost_observation)?
                    != *witness
                {
                    return Err(AppError::CursorExpired);
                }
            } else {
                let mut rows: Vec<Draft> = self
                    .read_note(&source, "draft_notes", None)
                    .await
                    .map_err(lost_observation)?;
                rows.sort_unstable_by_key(|row| row.id);
                if content_fingerprint(&rows)? != cached.fingerprint {
                    return Err(AppError::CursorExpired);
                }
            }
            self.discussions.check(&source, &scope, &observation)?;
            return Ok(DiscussionDrafts {
                observation: observation.id.clone(),
                items: cached.items.clone(),
                limited: cached.limited,
            });
        }
        let drafts = observation
            .drafts
            .get_or_try_init(|| async {
                let (account, user) = self.draft_account().await?;
                let mut rows: Vec<Draft> = self.read_note(&source, "draft_notes", None).await?;
                if rows.len() > MAX_DRAFTS {
                    return Err(AppError::PayloadTooLarge);
                }
                let mut seen = HashSet::new();
                rows.sort_unstable_by_key(|row| row.id);
                let fingerprint = content_fingerprint(&rows)?;
                let mut limited = false;
                let mut items = Vec::with_capacity(rows.len());
                for row in rows {
                    if !seen.insert(row.id) {
                        return Err(AppError::Unavailable);
                    }
                    match draft(row, id, account, &user.label, &revision) {
                        Ok(item) => items.push(item),
                        Err(AppError::PayloadTooLarge) => limited = true,
                        Err(error) => return Err(error),
                    }
                }
                self.confirm_discussion_context(&source, &revision.id, mr_id)
                    .await?;
                let weight = items.iter().map(cache::draft_bytes).sum();
                Ok::<_, AppError>(cache::Drafts {
                    items: Arc::from(items),
                    weight,
                    limited,
                    fingerprint,
                })
            })
            .await?;
        self.discussions.check(&source, &scope, &observation)?;
        observation.trim()?;
        Ok(DiscussionDrafts {
            observation: observation.id.clone(),
            items: drafts.items.clone(),
            limited: drafts.limited,
        })
    }

    pub(in crate::gitlab::changes) async fn read_discussion_target(
        &self,
        source: MrIdentity,
        scope: DiscussionScope,
        address: NoteAddress,
    ) -> Result<DiscussionTarget, AppError> {
        let (revision, mr_id) = self.discussion_context(&source, &scope.revision).await?;
        let observation = self.discussions.observe(&source, mr_id, &scope)?;
        let result = match address {
            NoteAddress::Published(id) => {
                if !valid_discussion_id(&id) {
                    return Err(AppError::CursorExpired);
                }
                let row: UpstreamDiscussion =
                    self.read_note(&source, "discussions", Some(&id)).await?;
                if row.id != id {
                    return Err(AppError::CursorExpired);
                }
                DiscussionTarget::Published(
                    project(row, &source, &revision)?.ok_or(AppError::NotFound)?,
                )
            }
            NoteAddress::Draft(id) => {
                let (account, user) = self.draft_account().await?;
                let mr_id = valid_id(mr_id).ok_or(AppError::Unavailable)?;
                let row: Draft = self
                    .read_note(&source, "draft_notes", Some(&id.to_string()))
                    .await?;
                if row.id != id {
                    return Err(AppError::CursorExpired);
                }
                let mut personal =
                    draft(row, mr_id, account, &user.label, &revision).map_err(lost_observation)?;
                if personal.anchor.is_none()
                    && let Some(parent) = &personal.parent
                {
                    let row: UpstreamDiscussion =
                        self.read_note(&source, "discussions", Some(parent)).await?;
                    if &row.id != parent {
                        return Err(AppError::CursorExpired);
                    }
                    let thread = project(row, &source, &revision)?.ok_or(AppError::NotFound)?;
                    personal.anchor = thread.anchor;
                    personal.outdated = thread.outdated;
                }
                DiscussionTarget::Draft(personal)
            }
        };
        self.confirm_discussion_context(&source, &revision.id, mr_id)
            .await?;
        self.discussions.check(&source, &scope, &observation)?;
        Ok(result)
    }

    async fn draft_account(&self) -> Result<(u64, Identity), AppError> {
        let account = self.account.ok_or(AppError::Forbidden)?;
        let user = self.current_user().await?;
        if user.id.parse::<u64>().ok() != Some(account) {
            return Err(AppError::Forbidden);
        }
        Ok((account, user))
    }

    async fn read_note<T: DeserializeOwned>(
        &self,
        source: &MrIdentity,
        collection: &str,
        id: Option<&str>,
    ) -> Result<T, AppError> {
        let mut url = self.url(&format!(
            "api/v4/projects/{}/merge_requests/{}/{collection}",
            source.project_id(),
            source.iid()
        ))?;
        if let Some(id) = id {
            url.path_segments_mut()
                .map_err(|_| AppError::Internal)?
                .push(id);
        }
        let (_, body) = self.get(url, MAX_DISCUSSION_BYTES).await?;
        serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)
    }
}

fn draft(
    wire: Draft,
    mr_id: u64,
    account: u64,
    author: &str,
    revision: &PatchRevision,
) -> Result<DiscussionDraft, AppError> {
    if wire.author_id != account || wire.merge_request_id != mr_id {
        return Err(AppError::Forbidden);
    }
    if valid_id(Some(wire.id)).is_none()
        || wire
            .discussion_id
            .as_deref()
            .is_some_and(|id| !valid_discussion_id(id))
    {
        return Err(AppError::Unavailable);
    }
    if wire.note.len() > 64 * 1024 {
        return Err(AppError::PayloadTooLarge);
    }
    let fingerprint = content_fingerprint(&wire)?;
    let position = wire.position.filter(|wire| {
        [
            "base_sha",
            "start_sha",
            "head_sha",
            "old_path",
            "new_path",
            "old_line",
            "new_line",
        ]
        .iter()
        .any(|field| !wire[field].is_null())
    });
    let anchor = position
        .map(|wire| {
            serde_json::from_value::<Position>(wire)
                .ok()
                .and_then(|position| position.anchor())
                .ok_or(AppError::Unavailable)
        })
        .transpose()?;
    let outdated = anchor.as_ref().is_some_and(|anchor| {
        anchor.head_sha != revision.head
            || anchor.start_sha != revision.start
            || anchor.base_sha != revision.base
    });
    Ok(DiscussionDraft {
        id: wire.id,
        fingerprint,
        account,
        author: author.to_owned(),
        body: wire.note,
        parent: wire.discussion_id,
        anchor,
        outdated,
    })
}

fn lost_observation(error: AppError) -> AppError {
    match error {
        AppError::Forbidden
        | AppError::Unauthorized
        | AppError::NotFound
        | AppError::PayloadTooLarge => AppError::CursorExpired,
        other => other,
    }
}

fn valid_id(value: Option<u64>) -> Option<u64> {
    value.filter(|id| (1..=i64::MAX as u64).contains(id))
}

fn valid_discussion_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
