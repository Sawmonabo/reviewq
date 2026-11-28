use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::changes::{
    DiscussionDraft, DiscussionInvalidation, DiscussionScope, DiscussionTarget, NoteAddress,
};
use reviewq_app::reading::displayed_markdown;
use reviewq_domain::identity::{EventCursor, ReviewId};
use serde::{Deserialize, Serialize};

use super::{
    ApiError, Entry, EventCursorDto, PageCursorDto, Path, Query, ReviewIdDto, SafeCountDto,
    SelectedSource, Success, check_discussions, discussion, dto, parse_review, patch_revision,
    project_anchor, read_source, valid_revision,
};

mod search;
pub(in crate::entry) use search::search_notes;

const HIT: &[u8] = b"reviewq-changes-note-hit-v1";

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Scope {
    review: String,
    revision: String,
    settings: u64,
    account: Option<u64>,
    epoch: u64,
    #[serde(skip)]
    journal_head: u64,
    pub(super) observation: Option<String>,
}

impl Scope {
    pub(super) fn new(
        review: &ReviewId,
        source: &SelectedSource,
        revision: String,
        epoch: EventCursor,
    ) -> Self {
        Self {
            review: review.to_string(),
            revision,
            settings: source.context.revision.get(),
            account: source.context.gitlab_account,
            epoch: epoch.get(),
            journal_head: source.cursor.max(epoch).get(),
            observation: None,
        }
    }

    pub(super) fn check_authority(&self, saved: &Self) -> Result<(), AppError> {
        if self.review != saved.review
            || self.revision != saved.revision
            || self.settings != saved.settings
            || self.account != saved.account
            || saved.observation.is_none()
        {
            return Err(AppError::CursorExpired);
        }
        Ok(())
    }

    pub(super) fn continued(self, mut saved: Self) -> Result<Self, AppError> {
        self.check_authority(&saved)?;
        if self.epoch != saved.epoch {
            return Err(AppError::CursorExpired);
        }
        saved.journal_head = self.journal_head;
        Ok(saved)
    }

    pub(super) fn provider(&self) -> Result<DiscussionScope, AppError> {
        Ok(DiscussionScope {
            revision: self.revision.clone(),
            epoch: EventCursor::try_from(self.epoch).map_err(|_| AppError::CursorExpired)?,
            journal_head: EventCursor::try_from(self.journal_head)
                .map_err(|_| AppError::Internal)?,
            observation: self.observation.clone(),
        })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Address {
    Published { thread: String, note: u64 },
    Draft { note: u64 },
}

#[derive(Deserialize, Serialize)]
struct Witness {
    address: Address,
    fingerprint: [u8; 32],
    matched: String,
    start: usize,
    end: usize,
    ordinal: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::entry) struct DestinationQuery {
    revision: String,
    hit: String,
    #[serde(default)]
    recheck: bool,
}

#[derive(Deserialize, Serialize)]
struct Hit {
    scope: Scope,
    witness: Witness,
}

pub(in crate::entry) async fn note_destination(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<DestinationQuery>,
) -> Result<Json<dto::DiscussionDestinationResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    if !valid_revision(&query.revision) {
        return Err(AppError::InvalidRequest.into());
    }
    let source = read_source(&entry, &actor, &review_id).await?;
    let epoch = entry
        .store
        .discussion_epoch(review_id.clone(), source.context.revision)
        .await?
        .cursor;
    let hit: Hit = entry.auth.endpoint.decode_cursor(HIT, &query.hit)?;
    let mut scope = Scope::new(&review_id, &source, query.revision, epoch).continued(hit.scope)?;
    if query.recheck {
        // Restore the confirmed reader position only after verifying the exact signed witness.
        scope.observation = None;
    }
    let witness = hit.witness;
    let address = match &witness.address {
        Address::Published { thread, .. } => NoteAddress::Published(thread.clone()),
        Address::Draft { note } => NoteAddress::Draft(*note),
    };
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    let target = provider
        .discussion_target(source.value.identity.clone(), scope.provider()?, address)
        .await;
    let destination = match target.and_then(|target| resolve(target, &witness, &source)) {
        Ok(destination) => destination,
        Err(error) if invalidates(&error) => {
            entry
                .store
                .invalidate_discussions(
                    review_id,
                    source.context.revision,
                    epoch,
                    DiscussionInvalidation::Changed,
                )
                .await?;
            return Err(error.into());
        }
        Err(error) => return Err(error.into()),
    };
    check_discussions(&entry, &actor, &review_id, &source, epoch).await?;
    Ok(Json(Success {
        cursor: event_cursor(source.cursor)?,
        data: dto::DiscussionDestination {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            revision: patch_revision(&scope.revision)?,
            thread: destination.0,
            note_id: destination.1,
            kind: destination.2,
            parent_discussion_id: destination.3,
            matched: witness.matched,
            start_utf16: count(witness.start)?,
            end_utf16: count(witness.end)?,
            ordinal: count(witness.ordinal)?,
        },
    }))
}

type Destination = (dto::PatchDiscussion, String, dto::NoteKind, Option<String>);

fn resolve(
    target: DiscussionTarget,
    witness: &Witness,
    source: &SelectedSource,
) -> Result<Destination, AppError> {
    let mr_url = source.value.web_url()?;
    match (target, &witness.address) {
        (DiscussionTarget::Published(thread), Address::Published { thread: id, note })
            if &thread.id == id && thread.fingerprint == witness.fingerprint =>
        {
            let confirmed = thread
                .notes
                .iter()
                .find(|item| item.id == *note)
                .ok_or(AppError::CursorExpired)?;
            verify_occurrence(&confirmed.body, witness)?;
            let kind = note_kind(thread.outdated, thread.anchor.is_some());
            Ok((
                discussion(&thread, &mr_url, &source.context)?,
                note.to_string(),
                kind,
                None,
            ))
        }
        (DiscussionTarget::Draft(draft), Address::Draft { note })
            if draft.id == *note && draft.fingerprint == witness.fingerprint =>
        {
            verify_occurrence(&draft.body, witness)?;
            let parent = draft.parent.clone();
            let note_id = format!("draft-{note}");
            Ok((
                draft_thread(draft, note_id.clone(), mr_url)?,
                note_id,
                dto::NoteKind::Draft,
                parent,
            ))
        }
        _ => Err(AppError::CursorExpired),
    }
}

fn verify_occurrence(source: &str, witness: &Witness) -> Result<(), AppError> {
    let displayed = displayed_markdown(source).ok_or(AppError::CursorExpired)?;
    if witness.end <= witness.start
        || !displayed
            .encode_utf16()
            .skip(witness.start)
            .take(witness.end - witness.start)
            .eq(witness.matched.encode_utf16())
    {
        return Err(AppError::CursorExpired);
    }
    Ok(())
}

fn draft_thread(
    draft: DiscussionDraft,
    note_id: String,
    web_url: String,
) -> Result<dto::PatchDiscussion, AppError> {
    Ok(dto::PatchDiscussion {
        discussion_id: note_id.clone(),
        anchor: draft.anchor.as_ref().map(project_anchor).transpose()?,
        outdated: draft.outdated,
        web_url,
        our_participation: true,
        resolution: dto::DiscussionResolution::Comment,
        notes: vec![dto::PatchDiscussionNote {
            note_id,
            web_url: String::new(),
            author: draft.author,
            author_id: reviewq_protocol::UpstreamIdDto::new(draft.account),
            ours: false,
            body: draft.body,
            created_at: None,
            resolvable: None,
            resolved: None,
            resolved_by: None,
        }],
    })
}

fn note_kind(outdated: bool, positioned: bool) -> dto::NoteKind {
    if outdated {
        dto::NoteKind::Outdated
    } else if positioned {
        dto::NoteKind::Inline
    } else {
        dto::NoteKind::General
    }
}

fn invalidates(error: &AppError) -> bool {
    error.refuses_access() || matches!(error, AppError::CursorExpired | AppError::NotFound)
}

fn count(value: usize) -> Result<SafeCountDto, AppError> {
    SafeCountDto::new(value as u64).ok_or(AppError::Internal)
}

fn event_cursor(cursor: EventCursor) -> Result<EventCursorDto, AppError> {
    EventCursorDto::new(cursor.get()).ok_or(AppError::Internal)
}

fn token<T: Serialize>(
    entry: &Entry,
    purpose: &[u8],
    position: &T,
) -> Result<PageCursorDto, AppError> {
    PageCursorDto::new(entry.auth.endpoint.encode_cursor(purpose, position)?)
        .ok_or(AppError::Internal)
}
