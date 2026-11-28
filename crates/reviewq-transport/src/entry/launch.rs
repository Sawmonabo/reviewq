use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::Snapshot;
use reviewq_app::launch as app;
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::{GitSha, ReviewId, Revision, SubmissionId};
use reviewq_domain::session::{AbsolutePath, HostProvider};
use reviewq_domain::settings::NativeSkill;
use reviewq_protocol::launch as dto;
use reviewq_protocol::settings::Skill;
use reviewq_protocol::{
    GitShaDto, HistoryEntryIdDto, HistoryTurnIdDto, HostProviderDto, ObservedInputDto,
    ResourceIdDto, ReviewIdDto, RevisionDto, RunIdDto, SessionKeyDto, SubmissionIdDto,
    UtcTimestampDto,
};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::error::{ApiBytes, ApiPath as Path};
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
            .map(|option| {
                Ok::<_, AppError>(dto::AvailableSkill {
                    name: option.skill.name,
                    path: path(&option.skill.path)?,
                    description: option.description,
                })
            })
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

pub(super) struct LaunchCommand {
    pub review: ReviewId,
    pub provider: HostProvider,
    pub submission: SubmissionId,
    pub selection: app::LaunchSelection,
    pub expected_revision: Option<Revision>,
    pub expected_sha: Option<GitSha>,
    pub digest: [u8; 32],
}

pub(super) async fn start(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::ReceiptResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input: dto::StartRequest = decode(&bytes)?;
    let canonical = serde_json::to_vec(&input).map_err(|_| AppError::Internal)?;
    let command = LaunchCommand {
        review: parse(input.review_id.as_str())?,
        provider: provider(input.provider),
        submission: parse(input.submission_id.as_str())?,
        selection: selection(
            Some(input.invocation),
            input.context,
            input.model,
            input.effort,
        )?,
        expected_revision: Some(
            Revision::try_from(input.expected_settings_revision.get())
                .map_err(|_| AppError::InvalidRequest)?,
        ),
        expected_sha: Some(parse(input.source_sha.as_str())?),
        digest: Sha256::digest(canonical).into(),
    };
    let stored = execute(&entry, actor, command).await?;
    Ok(Json(success(
        project_receipt(stored.value)?,
        stored.cursor,
    )?))
}

pub(super) async fn execute(
    entry: &Entry,
    actor: Actor,
    command: LaunchCommand,
) -> Result<Snapshot<app::LaunchReceipt>, AppError> {
    let LaunchCommand {
        review,
        provider,
        submission,
        selection,
        expected_revision,
        expected_sha,
        digest,
    } = command;
    if let Some(found) = entry
        .store
        .launch_receipt(
            actor.clone(),
            Some(review.clone()),
            submission.clone(),
            Some(digest),
        )
        .await?
    {
        return reconcile(entry, actor, found).await;
    }
    if !entry.sends_allowed {
        return Err(AppError::Forbidden);
    }
    let host = entry.launchers.select(provider)?;
    let slot = reviewq_app::wait::owned(entry.launch_slots.clone()).await?;
    let prepared = load(entry, actor.clone(), review.clone(), provider).await?;
    let input = prepared.value.select(selection)?;
    if expected_revision.is_some_and(|revision| revision != input.settings)
        || expected_sha
            .as_ref()
            .is_some_and(|sha| &input.source.head_sha != sha)
    {
        return Err(AppError::StaleRevision);
    }
    let (id, created_at_ms) = match entry
        .store
        .claim_launch(actor.clone(), input.clone(), submission, digest)
        .await?
    {
        app::LaunchClaim::Existing(found) => return reconcile(entry, actor, *found).await,
        app::LaunchClaim::Admitted { id, created_at_ms } => (id, created_at_ms),
    };
    let store = entry.store.clone();
    tokio::spawn(async move {
        let _slot = slot;
        app::run_launch(store, host, actor, id, created_at_ms, input).await
    })
    .await
    .map_err(|_| AppError::Internal)?
}

pub(super) async fn receipt(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((review, submission)): Path<(String, String)>,
) -> Result<Json<dto::ReceiptResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let stored = entry
        .store
        .launch_receipt(
            actor.clone(),
            Some(parse(&review)?),
            parse(&submission)?,
            None,
        )
        .await?
        .ok_or(AppError::NotFound)?;
    let stored = reconcile(&entry, actor, stored).await?;
    Ok(Json(success(
        project_receipt(stored.value)?,
        stored.cursor,
    )?))
}

pub(super) fn project_receipt(receipt: app::LaunchReceipt) -> Result<dto::Receipt, AppError> {
    Ok(dto::Receipt {
        review_id: ReviewIdDto::new(receipt.review.as_str()).ok_or(AppError::Internal)?,
        submission_id: SubmissionIdDto::new(receipt.submission.as_str())
            .ok_or(AppError::Internal)?,
        state: project_state(receipt.state),
        run_id: receipt
            .run
            .map(|run| RunIdDto::new(run.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        session_key: receipt
            .session
            .map(|session| SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        native_id: receipt
            .native
            .map(|native| ResourceIdDto::new(native.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        native_job_id: receipt
            .job
            .map(|id| ResourceIdDto::new(id).ok_or(AppError::Internal))
            .transpose()?,
        observed_input: receipt
            .observed_input
            .map(|input| {
                Ok::<_, AppError>(ObservedInputDto {
                    turn_id: HistoryTurnIdDto::new(input.turn_id.as_str())
                        .ok_or(AppError::Internal)?,
                    entry_id: HistoryEntryIdDto::new(input.entry_id.as_str())
                        .ok_or(AppError::Internal)?,
                })
            })
            .transpose()?,
        created_at: UtcTimestampDto::from_unix_milliseconds(receipt.created_at_ms)
            .ok_or(AppError::Internal)?,
        updated_at: UtcTimestampDto::from_unix_milliseconds(receipt.updated_at_ms)
            .ok_or(AppError::Internal)?,
    })
}

pub(super) async fn reconcile(
    entry: &Entry,
    actor: Actor,
    receipt: Snapshot<app::LaunchReceipt>,
) -> Result<Snapshot<app::LaunchReceipt>, AppError> {
    let value = &receipt.value;
    if value.state != app::LaunchState::Uncertain {
        return Ok(receipt);
    }
    if value.provider == HostProvider::Claude {
        let Some(locator) = value.claude.clone() else {
            return Ok(receipt);
        };
        let Ok(host) = entry.launchers.select(value.provider) else {
            return Ok(receipt);
        };
        let Ok(_slot) = entry.launch_slots.try_acquire() else {
            return Ok(receipt);
        };
        let Ok(Some((proof, job))) = host.reconcile_job(locator, value.created_at_ms).await else {
            return Ok(receipt);
        };
        return match entry
            .store
            .confirm_claude_launch(
                actor,
                value.review.clone(),
                value.submission.clone(),
                proof,
                job,
            )
            .await
        {
            Err(AppError::Conflict) => Ok(receipt),
            result => result,
        };
    }
    if value.run.is_none() {
        return Ok(receipt);
    }
    if value.observed_input.is_some() {
        return entry
            .store
            .finish_launch(value.id.clone(), app::NativeLaunchResult::Confirmed)
            .await;
    }
    let Ok(host) = entry.launchers.select(value.provider) else {
        return Ok(receipt);
    };
    let selected = entry
        .store
        .resolve_selection(actor.clone(), value.review.clone(), value.run.clone())
        .await?
        .value;
    let Some(target) = selected.lead().filter(|target| {
        Some(target.session_key()) == value.session.as_ref()
            && Some(target.binding().identity().native_id()) == value.native.as_ref()
    }) else {
        return Ok(receipt);
    };
    let Ok(Some(input)) = host.reconcile(target.clone(), value.id.clone()).await else {
        return Ok(receipt);
    };
    let current = entry
        .store
        .launch_receipt(
            actor,
            Some(value.review.clone()),
            value.submission.clone(),
            None,
        )
        .await?
        .ok_or(AppError::NotFound)?;
    if current.value.state != app::LaunchState::Uncertain {
        return Ok(current);
    }
    entry
        .store
        .record_launch_input(value.id.clone(), input)
        .await?;
    entry
        .store
        .finish_launch(value.id.clone(), app::NativeLaunchResult::Confirmed)
        .await
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
    let prior_source = stored.value.item.source;
    let identity = prior_source.identity.clone();
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
    let models = entry.models.reader(provider)?;
    let skills = entry.skills.reader(provider)?;
    reader
        .verify_launch_checkout(project, checkout.clone())
        .await?;
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
        || (current.value.item.source != prior_source && current.value.item.source != source)
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
            prior_source: current.value.item.source,
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
    let invocation = project_invocation(input.invocation)?;
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

pub(super) fn project_request_evidence(
    value: app::RequestEvidence,
) -> Result<dto::RequestEvidence, AppError> {
    let input = value
        .input
        .map(|input| {
            Ok::<_, AppError>(dto::RequestInput {
                slot: match input.slot {
                    app::RequestInputSlot::ReviewRequest => dto::RequestInputSlot::ReviewRequest,
                    app::RequestInputSlot::SkillArguments => dto::RequestInputSlot::SkillArguments,
                },
                turn_id: reviewq_protocol::HistoryTurnIdDto::new(input.turn.as_str())
                    .ok_or(AppError::Internal)?,
                entry_id: reviewq_protocol::HistoryEntryIdDto::new(input.entry.as_str())
                    .ok_or(AppError::Internal)?,
                generation: reviewq_protocol::HistoryGenerationDto::new(input.generation)
                    .ok_or(AppError::Internal)?,
            })
        })
        .transpose()?;
    Ok(dto::RequestEvidence {
        requested_model: value.requested_model,
        requested_effort: value.requested_effort,
        skill_name: value.skill_name,
        input,
    })
}

pub(super) fn project_run_launch(
    value: app::LaunchDetails,
) -> Result<dto::RunLaunchDetails, AppError> {
    Ok(dto::RunLaunchDetails {
        source_sha: GitShaDto::new(value.source.as_str()).ok_or(AppError::Internal)?,
        state: project_state(value.state),
        invocation: project_invocation(value.invocation)?,
        context: value.context,
        requested_model: value.requested_model,
        requested_effort: value.requested_effort,
        observed_model: value.observed_model,
        observed_effort: value.observed_effort,
    })
}

fn project_invocation(value: app::Invocation) -> Result<dto::Invocation, AppError> {
    Ok(match value {
        app::Invocation::Custom { prompt } => dto::Invocation::Custom { prompt },
        app::Invocation::Skill { skill, args } => dto::Invocation::Skill {
            name: skill.name,
            path: Some(path(&skill.path)?),
            args,
        },
    })
}

fn project_state(state: app::LaunchState) -> dto::State {
    match state {
        app::LaunchState::Pending => dto::State::Pending,
        app::LaunchState::Confirmed => dto::State::Confirmed,
        app::LaunchState::Rejected => dto::State::Rejected,
        app::LaunchState::Uncertain => dto::State::Uncertain,
    }
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
