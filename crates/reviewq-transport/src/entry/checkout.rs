use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::workspace::{GrantAddress, GrantBinding, IssueGrant};
use reviewq_domain::identity::{ProjectId, Revision};
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::settings::CheckoutBinding;
use reviewq_protocol::{RevisionDto, Success};
use serde::{Deserialize, Serialize};

use super::error::ApiBytes;
use super::pages::success;
use super::{ApiError, Entry, control_ops};

#[derive(Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "action",
    deny_unknown_fields
)]
enum Command {
    Applied { address: Address },
    Issue { address: Address },
    Revoke { common_dir: PathBuf },
    Status { address: Address },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Address {
    settings_revision: RevisionDto,
    project_id: String,
    checkout: PathBuf,
    common_dir: PathBuf,
    verifier: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", untagged)]
enum GrantResponse {
    Bound {
        #[serde(rename = "grantId")]
        grant_id: String,
        epoch: RevisionDto,
        #[serde(rename = "settingsRevision")]
        settings_revision: RevisionDto,
        origin: String,
        #[serde(rename = "projectId")]
        project_id: String,
        checkout: PathBuf,
        #[serde(rename = "commonDir")]
        common_dir: PathBuf,
    },
    Revoked {
        revoked: bool,
    },
}

pub(super) async fn grant(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let target = uri
        .path_and_query()
        .ok_or(AppError::InvalidRequest)?
        .as_str();
    let proof = entry.auth.control("POST", target, &headers, &bytes)?;
    let input = serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest);
    let result = match input {
        Ok(input) => apply(&entry, input).await,
        Err(error) => Err(error),
    };
    control_ops::signed(&entry, &proof, StatusCode::OK, result)
}

async fn apply(entry: &Entry, input: Command) -> Result<Success<GrantResponse>, AppError> {
    let actor = Actor::local_operator();
    match input {
        Command::Issue { address } => {
            let address = read_address(address).await?;
            let configured = entry.store.workspace_context(actor.clone()).await?.value;
            let active = entry.active_source().await?;
            if active.revision != address.settings || configured.revision != address.settings {
                return Err(AppError::StaleRevision);
            }
            let mut scope = configured
                .projects
                .into_iter()
                .filter(|candidate| candidate.project_id == address.project);
            let selected = scope.next().ok_or(AppError::NotFound)?;
            if scope.next().is_some() {
                return Err(AppError::Conflict);
            }
            let source = active.reader.ok_or(AppError::Unavailable)?;
            let checkout = CheckoutBinding {
                root: address.checkout.clone(),
                common_dir: address.common_dir.clone(),
            };
            source
                .verify_launch_checkout(selected.clone(), checkout)
                .await?;
            let stored = entry
                .store
                .issue_grant(
                    actor,
                    IssueGrant {
                        settings: address.settings,
                        project: selected,
                        checkout: address.checkout,
                        common_dir: address.common_dir,
                        verifier: address.verifier,
                        operator_requested: true,
                    },
                )
                .await?;
            success(project(stored.value, address.settings)?, stored.cursor)
        }
        Command::Status { address } => {
            let address = read_address(address).await?;
            let settings = address.settings;
            let stored = entry.store.granted(actor, address).await?;
            success(project(stored.value, settings)?, stored.cursor)
        }
        Command::Applied { address } => {
            let address = read_address(address).await?;
            let settings = address.settings;
            let stored = entry.store.confirm_grant_projection(actor, address).await?;
            success(project(stored.value, settings)?, stored.cursor)
        }
        Command::Revoke { common_dir } => {
            let path =
                AbsolutePath::from_canonical(common_dir).map_err(|_| AppError::InvalidRequest)?;
            let stored = entry.store.revoke_grant(actor, path).await?;
            success(
                GrantResponse::Revoked {
                    revoked: stored.value,
                },
                stored.cursor,
            )
        }
    }
}

fn project(binding: GrantBinding, settings: Revision) -> Result<GrantResponse, AppError> {
    Ok(GrantResponse::Bound {
        grant_id: binding.id.to_string(),
        epoch: RevisionDto::new(binding.epoch.get()).ok_or(AppError::Internal)?,
        settings_revision: RevisionDto::new(settings.get()).ok_or(AppError::Internal)?,
        origin: binding.issuing_project.origin.as_str().to_owned(),
        project_id: binding.issuing_project.project_id.to_string(),
        checkout: binding.checkout.as_path().to_owned(),
        common_dir: binding.common_dir.as_path().to_owned(),
    })
}

async fn read_address(address: Address) -> Result<GrantAddress, AppError> {
    Ok(GrantAddress {
        settings: Revision::try_from(address.settings_revision.get())
            .map_err(|_| AppError::InvalidRequest)?,
        project: address
            .project_id
            .parse::<ProjectId>()
            .map_err(|_| AppError::InvalidRequest)?,
        checkout: directory(address.checkout).await?,
        common_dir: directory(address.common_dir).await?,
        verifier: URL_SAFE_NO_PAD
            .decode(address.verifier)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(AppError::InvalidRequest)?,
    })
}

async fn directory(path: PathBuf) -> Result<AbsolutePath, AppError> {
    let real = tokio::fs::canonicalize(&path)
        .await
        .map_err(|_| AppError::NotFound)?;
    let metadata = tokio::fs::metadata(&real)
        .await
        .map_err(|_| AppError::NotFound)?;
    if real != path || !metadata.is_dir() {
        return Err(AppError::InvalidRequest);
    }
    AbsolutePath::from_canonical(real).map_err(|_| AppError::InvalidRequest)
}
