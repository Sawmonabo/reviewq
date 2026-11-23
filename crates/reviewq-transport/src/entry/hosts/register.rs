use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::hosts::{authorize_registration, register_proven};
use reviewq_app::launch::report_guidance;
use reviewq_domain::identity::NativeSessionId;
use reviewq_domain::session::HostProvider;
use reviewq_protocol::registration::{
    RegisterSessionRequest, RegisteredSession, RegisteredSessionResponse,
};
use reviewq_protocol::{HostProviderDto, RunIdDto, SessionKeyDto};

use crate::entry::error::ApiBytes;
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

pub(in crate::entry) async fn register(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<RegisteredSessionResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    if bytes.len() > 4096 {
        return Err(AppError::PayloadTooLarge.into());
    }
    let input: RegisterSessionRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let review_id = parse(input.review_id.as_str())?;
    let native: NativeSessionId = parse(input.native_id.as_str())?;
    let (revision, mr) = authorize_registration(entry.store.as_ref(), actor, review_id).await?;
    let source = entry.active_source().await?;
    if source.revision != revision {
        return Err(AppError::StaleRevision.into());
    }
    let checkout = source
        .scope
        .repositories
        .iter()
        .find(|repository| repository.project == mr.project_id())
        .and_then(|repository| repository.checkout.as_ref())
        .map(|checkout| checkout.root.as_path().to_string_lossy().into_owned());
    let reader = source.reader.ok_or(AppError::Unavailable)?;
    let (provider, proof) = match input.provider {
        HostProviderDto::Codex => (
            HostProvider::Codex,
            entry
                .host
                .as_ref()
                .ok_or(AppError::Unavailable)?
                .prove(native.clone())
                .await?,
        ),
        HostProviderDto::Claude => (
            HostProvider::Claude,
            entry
                .claude
                .as_ref()
                .ok_or(AppError::Unavailable)?
                .inspect_lead(native.clone())
                .await?,
        ),
    };
    if proof.binding.identity().provider() != provider
        || proof.binding.identity().native_id() != &native
    {
        return Err(AppError::Conflict.into());
    }
    let remote = reader.review(mr.clone()).await?;
    if remote.identity != mr {
        return Err(AppError::Conflict.into());
    }
    let registered = register_proven(entry.store.as_ref(), proof, remote, revision).await?;
    let lead = registered.value;
    Ok(Json(success(
        RegisteredSession {
            review_id: input.review_id,
            run_id: RunIdDto::new(lead.run_id().as_str()).ok_or(AppError::Internal)?,
            lead_session_key: SessionKeyDto::new(lead.session_key().as_str())
                .ok_or(AppError::Internal)?,
            provider: input.provider,
            native_id: input.native_id,
            cwd: lead
                .binding()
                .cwd()
                .as_path()
                .to_string_lossy()
                .into_owned(),
            review_checkout: checkout,
            report_guidance: report_guidance(),
        },
        registered.cursor,
    )?))
}
