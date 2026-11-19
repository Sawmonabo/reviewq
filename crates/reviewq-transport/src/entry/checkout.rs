use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::workspace::IssueGrant;
use reviewq_domain::identity::ProjectId;
use reviewq_domain::session::AbsolutePath;
use serde::Deserialize;
use serde_json::json;

use super::error::ApiBytes;
use super::pages::success;
use super::{ApiError, Entry};

#[derive(Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "action",
    deny_unknown_fields
)]
enum Command {
    Issue {
        project_id: String,
        checkout: PathBuf,
        common_dir: PathBuf,
        remote: String,
        verifier: String,
    },
    Revoke {
        common_dir: PathBuf,
    },
    Status {
        verifier: String,
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
    let input: Command = serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let actor = Actor::local_operator();
    let output = match input {
        Command::Issue {
            project_id,
            checkout,
            common_dir,
            remote,
            verifier,
        } => {
            let project_id: ProjectId = project_id.parse().map_err(|_| AppError::InvalidRequest)?;
            let workflow = entry.store.workspace_context(actor.clone()).await?.value;
            let mut scope = workflow
                .projects
                .into_iter()
                .filter(|candidate| candidate.project_id == project_id);
            let selected = scope.next().ok_or(AppError::NotFound)?;
            if scope.next().is_some() {
                return Err(AppError::Conflict.into());
            }
            let source = entry.source_reader().await?;
            let selected = source.verify_checkout(selected, remote).await?;
            let checksum = URL_SAFE_NO_PAD
                .decode(verifier)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or(AppError::InvalidRequest)?;
            let input = IssueGrant {
                project: selected,
                checkout: directory(checkout).await?,
                common_dir: directory(common_dir).await?,
                verifier: checksum,
            };
            let stored = entry.store.issue_grant(actor, input).await?;
            success(
                json!({"grantId":stored.value.id.as_str(),"epoch":stored.value.epoch.to_string()}),
                stored.cursor,
            )?
        }
        Command::Revoke { common_dir } => {
            let path =
                AbsolutePath::from_canonical(common_dir).map_err(|_| AppError::InvalidRequest)?;
            let removed = entry.store.revoke_grant(actor, path).await?;
            success(json!({"revoked":removed.value}), removed.cursor)?
        }
        Command::Status { verifier } => {
            let key = URL_SAFE_NO_PAD
                .decode(verifier)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or(AppError::InvalidRequest)?;
            let active = match entry.store.authenticate_grant(key).await {
                Ok(_) => true,
                Err(AppError::Unauthorized) => false,
                Err(error) => return Err(error.into()),
            };
            let current = entry.store.workspace_context(actor).await?;
            success(json!({"active":active}), current.cursor)?
        }
    };
    let bytes = serde_json::to_vec(&output).map_err(|_| AppError::Internal)?;
    let mut response = HeaderMap::new();
    response.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    entry
        .auth
        .endpoint
        .response_proof(&proof, 200, &mut response, &bytes)?;
    Ok((response, bytes).into_response())
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
