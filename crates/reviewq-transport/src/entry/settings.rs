use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::Snapshot;
use reviewq_app::settings as app;
use reviewq_domain::identity::Revision;
use reviewq_domain::settings::{Connection, Identity, JiraTokenType, Provider};
use reviewq_protocol::settings as dto;
use reviewq_protocol::{PageCursorDto, ResourceIdDto, RevisionDto};
use sha2::{Digest, Sha256};

use super::error::ApiBytes;
use super::pages::{parse, success};
use super::{ApiError, Entry};

mod scope;

pub(super) async fn read(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
) -> Result<Json<dto::SettingsResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    Ok(Json(view(entry.store.settings(actor).await?)?))
}

pub(super) async fn test(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::VerifiedConnectionResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let request: dto::TestConnection = decode(&bytes)?;
    let expected = revision(request.expected_revision)?;
    let saved = entry.store.read_credentials(actor.clone()).await?;
    let draft = draft(request.provider, request.connection, request.token)?;
    let proof = app::verify_connection(
        entry.settings_sources.as_ref(),
        &saved.value,
        expected,
        draft,
    )
    .await?;
    let current = revision_snapshot(&entry, actor, expected).await?;
    Ok(Json(success(
        dto::VerifiedConnection {
            provider: request.provider,
            identity: identity(proof.account.identity)?,
        },
        current.cursor,
    )?))
}

pub(super) async fn catalog(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::CatalogResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let request: dto::CatalogRequest = decode(&bytes)?;
    let expected = revision(request.expected_revision)?;
    let saved = entry.store.read_credentials(actor.clone()).await?;
    let kind = catalog_kind(request.kind, request.board_id, request.view)?;
    let mut query = app::CatalogQuery {
        kind,
        text: request.q,
        states: request.states,
        offset: 0,
    };
    if query.states.len() > 3
        || query
            .text
            .as_ref()
            .is_some_and(|value| value.chars().take(201).count() > 200)
    {
        return Err(AppError::InvalidRequest.into());
    }
    let draft = draft(request.provider, request.connection, request.token)?;
    let authority = app::source_authority(
        entry.settings_sources.as_ref(),
        &saved.value,
        expected,
        draft,
    )?;
    let fingerprint = Sha256::digest(authority.token.expose().as_bytes());
    let scope = serde_json::to_vec(&(
        "settings-catalog",
        expected.get(),
        format!("{:?}", authority.connection),
        format!("{:?}", query.kind),
        &query.text,
        &query.states,
        fingerprint.as_slice(),
    ))
    .map_err(|_| AppError::Internal)?;
    let endpoint = &entry.auth.endpoint;
    if let Some(cursor) = &request.cursor {
        query.offset = endpoint.decode_cursor(&scope, cursor.as_str())?;
    }
    let found = entry.settings_sources.catalog(authority, query).await?;
    let current = revision_snapshot(&entry, actor, expected).await?;
    let next_cursor = found
        .next
        .map(|next| {
            let cursor = endpoint.encode_cursor(&scope, &next)?;
            PageCursorDto::new(cursor).ok_or(AppError::Internal)
        })
        .transpose()?;
    let items = found
        .items
        .into_iter()
        .map(catalog_item)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(success(
        dto::CatalogResult { items, next_cursor },
        current.cursor,
    )?))
}

pub(super) async fn save(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::SettingsResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let request: dto::SaveSettings = decode(&bytes)?;
    let expected = revision(request.expected_revision)?;
    let saved = entry.store.read_credentials(actor.clone()).await?;
    let workspace = scope::draft(
        request.draft.boards,
        request.draft.repositories,
        request.draft.additional_reviewer_ids,
    )?;
    let (gitlab, jira) = tokio::try_join!(
        app::verify_connection(
            entry.settings_sources.as_ref(),
            &saved.value,
            expected,
            draft(
                dto::Provider::Gitlab,
                dto::ConnectionDraft::Gitlab(request.draft.gitlab),
                request.tokens.gitlab
            )?
        ),
        app::verify_connection(
            entry.settings_sources.as_ref(),
            &saved.value,
            expected,
            draft(
                dto::Provider::Jira,
                dto::ConnectionDraft::Jira(request.draft.jira),
                request.tokens.jira
            )?
        ),
    )?;
    let verified = entry
        .settings_sources
        .verify_scope(&gitlab, &jira, workspace)
        .await?;
    entry.skills.verify(&verified).await?;
    let stored = entry
        .store
        .activate_connections(actor, expected, gitlab, jira, verified)
        .await?;
    entry.refresh_wakeup.notify_one();
    Ok(Json(view(stored)?))
}

pub(super) async fn disconnect(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::SettingsResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let request: dto::Disconnect = decode(&bytes)?;
    let expected = revision(request.expected_revision)?;
    Ok(Json(view(
        entry
            .store
            .disconnect(actor, expected, provider(request.provider))
            .await?,
    )?))
}

async fn revision_snapshot(
    entry: &Entry,
    actor: Actor,
    expected: Revision,
) -> Result<Snapshot<app::Connections>, AppError> {
    let current = entry.store.settings(actor).await?;
    if current.value.revision != expected {
        return Err(AppError::StaleRevision);
    }
    Ok(current)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, AppError> {
    serde_json::from_slice(bytes).map_err(|_| AppError::InvalidRequest)
}

fn revision(value: RevisionDto) -> Result<Revision, AppError> {
    Revision::try_from(value.get()).map_err(|_| AppError::InvalidRequest)
}

fn provider(value: dto::Provider) -> Provider {
    match value {
        dto::Provider::Gitlab => Provider::GitLab,
        dto::Provider::Jira => Provider::Jira,
    }
}

fn draft(
    provider_value: dto::Provider,
    input: dto::ConnectionDraft,
    token: dto::TokenInput,
) -> Result<app::DraftConnection, AppError> {
    let connection = match input {
        dto::ConnectionDraft::Gitlab(value) => Connection::GitLab {
            origin: value.origin,
        },
        dto::ConnectionDraft::Jira(value) => Connection::Jira {
            site: value.site,
            email: value.email,
            token_type: match value.token_type {
                dto::TokenType::Scoped => JiraTokenType::Scoped,
                dto::TokenType::Unscoped => JiraTokenType::Unscoped,
            },
        },
    };
    if connection.provider() != provider(provider_value) {
        return Err(AppError::InvalidRequest);
    }
    let token = match token {
        dto::TokenInput::Keep => app::TokenChange::Keep,
        dto::TokenInput::Replace { value } => {
            app::TokenChange::Replace(app::SecretToken::new(value)?)
        }
    };
    Ok(app::DraftConnection { connection, token })
}

fn catalog_kind(
    kind: dto::CatalogKind,
    board: Option<ResourceIdDto>,
    view: Option<dto::ProjectView>,
) -> Result<app::CatalogKind, AppError> {
    use app::{CatalogKind as Kind, ProjectView as View};
    let kind = match (kind, board, view) {
        (dto::CatalogKind::Projects, None, view) => Kind::Projects(match view {
            None | Some(dto::ProjectView::Member) => View::Member,
            Some(dto::ProjectView::Starred) => View::Starred,
            Some(dto::ProjectView::All) => View::All,
        }),
        (dto::CatalogKind::Boards, None, None) => Kind::Boards,
        (dto::CatalogKind::Board, Some(board), None) => Kind::Board(parse(board.as_str())?),
        (dto::CatalogKind::Statuses, None, None) => Kind::Statuses,
        (dto::CatalogKind::Filters, None, None) => Kind::Filters,
        (dto::CatalogKind::Sprints, Some(board), None) => Kind::Sprints(parse(board.as_str())?),
        _ => return Err(AppError::InvalidRequest),
    };
    Ok(kind)
}

fn view(snapshot: Snapshot<app::Connections>) -> Result<dto::SettingsResponse, AppError> {
    let app::Connections {
        revision,
        gitlab,
        jira,
        scope,
        chat_setup_pending,
    } = snapshot.value;
    let mode = if gitlab.is_none() && jira.is_none() {
        dto::Mode::Setup
    } else if gitlab.as_ref().is_some_and(|view| view.configured)
        && jira.as_ref().is_some_and(|view| view.configured)
    {
        dto::Mode::Connected
    } else {
        dto::Mode::Disconnected
    };
    let gitlab = match gitlab {
        Some(summary) => {
            let Connection::GitLab { origin } = summary.account.connection else {
                return Err(AppError::Internal);
            };
            dto::GitlabConnection {
                origin: Some(origin),
                configured: summary.configured,
                identity: Some(identity(summary.account.identity)?),
            }
        }
        None => dto::GitlabConnection::default(),
    };
    let jira = match jira {
        Some(summary) => {
            let Connection::Jira {
                site,
                email,
                token_type,
            } = summary.account.connection
            else {
                return Err(AppError::Internal);
            };
            let token_type = match token_type {
                JiraTokenType::Scoped => dto::TokenType::Scoped,
                JiraTokenType::Unscoped => dto::TokenType::Unscoped,
            };
            dto::JiraConnection {
                site: Some(site),
                email: Some(email),
                token_type: Some(token_type),
                configured: summary.configured,
                identity: Some(identity(summary.account.identity)?),
            }
        }
        None => dto::JiraConnection::default(),
    };
    success(
        dto::SettingsView {
            settings_revision: RevisionDto::new(revision.get()).ok_or(AppError::Internal)?,
            mode,
            connections: dto::Connections { gitlab, jira },
            boards: scope
                .boards
                .into_iter()
                .map(scope::board)
                .collect::<Result<_, _>>()?,
            repositories: scope
                .repositories
                .into_iter()
                .map(scope::repo)
                .collect::<Result<_, _>>()?,
            additional_reviewer_ids: scope::number_dtos(scope.additional_reviewers)?,
            chat_setup: dto::ChatSetup {
                state: if chat_setup_pending {
                    dto::ChatSetupState::Pending
                } else {
                    dto::ChatSetupState::Ready
                },
                issues: Vec::new(),
            },
        },
        snapshot.cursor,
    )
}

fn identity(value: Identity) -> Result<dto::Identity, AppError> {
    Ok(dto::Identity {
        id: resource(value.id)?,
        label: value.label,
    })
}

fn resource(value: String) -> Result<ResourceIdDto, AppError> {
    ResourceIdDto::new(value).ok_or(AppError::Internal)
}

fn catalog_item(value: app::CatalogItem) -> Result<dto::CatalogItem, AppError> {
    let board_type = match value.board_type.as_deref() {
        Some("scrum") => Some(dto::BoardType::Scrum),
        Some("kanban") => Some(dto::BoardType::Kanban),
        None => None,
        _ => return Err(AppError::Internal),
    };
    Ok(dto::CatalogItem {
        id: resource(value.id)?,
        label: value.label,
        path: value.path,
        board_id: value
            .board_id
            .map(|id| resource(id.to_string()))
            .transpose()?,
        state: value.state,
        board_type,
        status_ids: value
            .status_ids
            .into_iter()
            .map(resource)
            .collect::<Result<Vec<_>, _>>()?,
        estimate_field: value.estimate_field,
    })
}
