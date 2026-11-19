use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::Snapshot;
use reviewq_app::launch as app;
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::ReviewId;
use reviewq_domain::session::{AbsolutePath, HostProvider};
use reviewq_domain::settings::NativeSkill;
use reviewq_protocol::launch as dto;
use reviewq_protocol::settings::Skill;
use reviewq_protocol::{GitShaDto, HostProviderDto, ReviewIdDto, RevisionDto};
use serde::de::DeserializeOwned;

use super::error::ApiBytes;
use super::pages::{parse, success};
use super::{ApiError, Entry};

pub(super) async fn catalog(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::CatalogResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let request: dto::CatalogRequest = decode(&bytes)?;
    let found = load(
        &entry,
        actor,
        parse(request.review_id.as_str())?,
        provider(request.provider),
    )
    .await?;
    let value = found.value;
    let result = dto::Catalog {
        review_id: request.review_id,
        provider: request.provider,
        settings_revision: RevisionDto::new(value.settings.get()).ok_or(AppError::Internal)?,
        checkout: path(&value.checkout.root)?,
        default_skill: value.default_skill.map(skill).transpose()?,
        skills: value
            .skills
            .into_iter()
            .map(skill)
            .collect::<Result<_, _>>()?,
        models: value
            .models
            .into_iter()
            .map(|value| dto::Model {
                id: value.id,
                label: value.label,
                is_default: value.is_default,
                default_effort: value.default_effort,
                efforts: value.efforts,
            })
            .collect(),
    };
    Ok(Json(success(result, found.cursor)?))
}

pub(super) async fn prepare(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::PreviewResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let request: dto::PrepareRequest = decode(&bytes)?;
    let selection = selection(
        request.invocation,
        request.context,
        request.model,
        request.effort,
    )?;
    let prepared = load(
        &entry,
        actor,
        parse(request.review_id.as_str())?,
        provider(request.provider),
    )
    .await?;
    let input = prepared.value.select(selection)?;
    Ok(Json(success(preview(input)?, prepared.cursor)?))
}

pub(super) async fn load(
    entry: &Entry,
    actor: Actor,
    review: ReviewId,
    provider: HostProvider,
) -> Result<Snapshot<app::LaunchPreparation>, AppError> {
    let active = entry.active_source().await?;
    let reader = active.reader.ok_or(AppError::Unavailable)?;
    let stored = entry
        .store
        .read_review(actor.clone(), review.clone(), None, 1)
        .await?;
    if stored.value.settings_revision != active.revision {
        return Err(AppError::StaleRevision);
    }
    let identity = stored.value.item.source.identity;
    let repository = active
        .scope
        .repositories
        .iter()
        .find(|repo| repo.project == identity.project_id())
        .ok_or(AppError::NotFound)?;
    let checkout = repository
        .checkout
        .clone()
        .ok_or(AppError::PreconditionFailed)?;
    let project = ConfiguredProject {
        origin: identity.origin().clone(),
        project_id: identity.project_id(),
    };
    reader
        .verify_launch_checkout(project, checkout.clone())
        .await?;
    let models = entry.models.reader(provider)?;
    let skills = entry.skills.reader(provider)?;
    let (source, models, skills) = tokio::try_join!(
        reader.review(identity.clone()),
        models.models(checkout.root.clone()),
        skills.skills(checkout.root.clone()),
    )?;
    if source.identity != identity {
        return Err(AppError::Conflict);
    }
    let current = entry
        .store
        .read_review(actor, review.clone(), None, 1)
        .await?;
    if current.value.settings_revision != active.revision
        || current.value.item.source.identity != identity
    {
        return Err(AppError::StaleRevision);
    }
    let default_skill = match provider {
        HostProvider::Codex => repository.default_skills.codex.clone(),
        HostProvider::Claude => repository.default_skills.claude.clone(),
    };
    Ok(Snapshot {
        cursor: current.cursor,
        value: app::LaunchPreparation {
            review,
            source,
            settings: active.revision,
            provider,
            checkout,
            default_skill,
            models,
            skills,
        },
    })
}

pub(super) fn selection(
    invocation: Option<dto::Invocation>,
    context: Option<String>,
    model: Option<String>,
    effort: Option<String>,
) -> Result<app::LaunchSelection, AppError> {
    let invocation = match invocation {
        Some(dto::Invocation::Custom { prompt }) => Some(app::InvocationDraft::Custom { prompt }),
        Some(dto::Invocation::Skill { name, path, args }) => Some(app::InvocationDraft::Skill {
            name,
            args,
            path: path
                .map(|value| {
                    if value.len() > 4096 || value.contains('\0') {
                        return Err(AppError::InvalidRequest);
                    }
                    AbsolutePath::from_canonical(value.into()).map_err(|_| AppError::InvalidRequest)
                })
                .transpose()?,
        }),
        None => None,
    };
    Ok(app::LaunchSelection {
        invocation,
        context,
        model,
        effort,
    })
}

fn preview(input: app::LaunchInput) -> Result<dto::Preview, AppError> {
    let prompt = input.prompt();
    let invocation = match input.invocation {
        app::Invocation::Custom { prompt } => dto::Invocation::Custom { prompt },
        app::Invocation::Skill { skill, args } => dto::Invocation::Skill {
            name: skill.name,
            path: Some(path(&skill.path)?),
            args,
        },
    };
    Ok(dto::Preview {
        review_id: ReviewIdDto::new(input.review.as_str()).ok_or(AppError::Internal)?,
        provider: match input.provider {
            HostProvider::Codex => HostProviderDto::Codex,
            HostProvider::Claude => HostProviderDto::Claude,
        },
        settings_revision: RevisionDto::new(input.settings.get()).ok_or(AppError::Internal)?,
        source_sha: GitShaDto::new(input.source.head_sha.as_str()).ok_or(AppError::Internal)?,
        checkout: path(&input.checkout.root)?,
        invocation,
        context: input.context,
        model: input.model,
        effort: input.effort,
        prompt,
    })
}

fn skill(value: NativeSkill) -> Result<Skill, AppError> {
    Ok(Skill {
        name: value.name,
        path: path(&value.path)?,
    })
}

fn path(value: &AbsolutePath) -> Result<String, AppError> {
    value
        .as_path()
        .to_str()
        .map(str::to_owned)
        .ok_or(AppError::Internal)
}

pub(super) fn provider(value: HostProviderDto) -> HostProvider {
    match value {
        HostProviderDto::Codex => HostProvider::Codex,
        HostProviderDto::Claude => HostProvider::Claude,
    }
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AppError> {
    serde_json::from_slice(bytes).map_err(|_| AppError::InvalidRequest)
}
