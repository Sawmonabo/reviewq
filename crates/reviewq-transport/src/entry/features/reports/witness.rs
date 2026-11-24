use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use reviewq_app::access::AppError;
use reviewq_domain::identity::{NativeTurnId, SessionKey};
use reviewq_protocol::ResourceIdDto;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::entry::error::ApiBytes;
use crate::entry::pages::parse;
use crate::entry::{ApiError, Entry};

const LIMIT: usize = 256;
const RETENTION: Duration = Duration::from_secs(10 * 60);

struct Observation {
    session: SessionKey,
    turn: NativeTurnId,
    call: String,
    digest: [u8; 32],
    ordinal: u32,
    seen: Instant,
}

#[derive(Default)]
pub(in crate::entry) struct ReportWitnesses(Mutex<Vec<Observation>>);

impl ReportWitnesses {
    fn record(
        &self,
        mut observation: Observation,
        stored: (u32, Option<u32>),
    ) -> Result<(), AppError> {
        let mut observations = self.0.lock().map_err(|_| AppError::Internal)?;
        observations.retain(|item| item.seen.elapsed() < RETENTION);
        let mut largest = stored.0;
        for item in &mut *observations {
            if item.session != observation.session {
                continue;
            }
            if item.call == observation.call {
                if item.digest != observation.digest || item.turn != observation.turn {
                    return Err(AppError::Conflict);
                }
                item.seen = observation.seen;
                return Ok(());
            }
            if item.turn == observation.turn {
                largest = largest.max(item.ordinal);
            }
        }
        if observations.len() == LIMIT {
            return Err(AppError::Busy);
        }
        observation.ordinal = match stored.1 {
            Some(ordinal) => ordinal,
            None => largest.checked_add(1).ok_or(AppError::Busy)?,
        };
        observations.push(observation);
        Ok(())
    }

    pub(in crate::entry) fn take_proof(
        &self,
        session: &SessionKey,
        call: &str,
        arguments: &[u8],
    ) -> Result<Option<(NativeTurnId, u32)>, AppError> {
        let observations = self.0.lock().map_err(|_| AppError::Internal)?;
        let Some(item) = observations.iter().find(|item| {
            &item.session == session && item.call == call && item.seen.elapsed() < RETENTION
        }) else {
            return Ok(None);
        };
        let digest: [u8; 32] = Sha256::digest(arguments).into();
        if item.digest != digest {
            return Err(AppError::Conflict);
        }
        Ok(Some((item.turn.clone(), item.ordinal)))
    }
}

pub(in crate::entry) fn canonical_arguments(mut value: Value) -> Result<Vec<u8>, AppError> {
    value.sort_all_objects();
    serde_json::to_vec(&value).map_err(|_| AppError::InvalidRequest)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Hook {
    session_id: ResourceIdDto,
    turn_id: ResourceIdDto,
    call_id: ResourceIdDto,
    arguments: Value,
}

pub(in crate::entry::features) async fn record(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<StatusCode, ApiError> {
    super::handoff::authenticate(&entry, &headers)?;
    let hook: Hook = serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let session: SessionKey = parse(&format!("codex:{}", hook.session_id.as_str()))?;
    let turn: NativeTurnId = parse(hook.turn_id.as_str())?;
    let call = hook.call_id.as_str().to_owned();
    let arguments = canonical_arguments(hook.arguments)?;
    if arguments.len() > reviewq_app::reports::METADATA_BYTES {
        return Err(AppError::PayloadTooLarge.into());
    }
    let stored = entry
        .store
        .report_hook_order(session.clone(), turn.clone(), call.clone())
        .await?;
    entry.report_witnesses.record(
        Observation {
            session,
            turn,
            call,
            digest: Sha256::digest(arguments).into(),
            ordinal: 0,
            seen: Instant::now(),
        },
        stored,
    )?;
    Ok(StatusCode::NO_CONTENT)
}
