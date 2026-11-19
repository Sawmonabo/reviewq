use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reviewq_app::access::AppError;
use reviewq_app::hosts::recovery::{InterruptState, RecoveryAuthorization, RecoveryKind};
use reviewq_app::workflow::{ReadyWorkflow, WorkflowReadiness};
use reviewq_domain::identity::{NativeTurnId, ReportHash};
use reviewq_domain::session::LeadTarget;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::Codex;
use super::session::ThreadStatus;

pub(super) fn classify(error: &Value) -> Option<RecoveryKind> {
    match error.as_str() {
        Some("usageLimitExceeded") => Some(RecoveryKind::UsageLimit),
        Some("serverOverloaded" | "flexUnavailable" | "policyCapacityUnavailable") => {
            Some(RecoveryKind::ModelCapacity)
        }
        Some("unauthorized") => Some(RecoveryKind::Access),
        _ => None,
    }
}

impl Codex {
    pub(super) async fn capacity(&self, fresh: bool) -> Result<Usage, AppError> {
        let mut cached = self.capacity.lock().await;
        if !fresh
            && let Some((at, usage)) = cached.as_ref()
            && at.elapsed() < Duration::from_secs(3)
        {
            return Ok(usage.clone());
        }
        let observed: Result<Usage, _> = self
            .connection
            .read(
                "account/rateLimits/read",
                json!({"excludeResetCreditDetails":true}),
            )
            .await;
        *cached = observed
            .as_ref()
            .ok()
            .map(|usage| (Instant::now(), usage.clone()));
        observed
    }

    pub(crate) async fn prepare_continuation(
        &self,
        lead: &LeadTarget,
        episode: String,
        failed_turn: NativeTurnId,
        kind: RecoveryKind,
        account: Option<ReportHash>,
        provider: Option<String>,
        fresh: bool,
    ) -> Result<(RecoveryAuthorization, ReadyWorkflow), AppError> {
        let account = account.ok_or(AppError::Unavailable)?;
        let model_provider = provider
            .filter(|value| value == "openai")
            .ok_or(AppError::Unavailable)?;
        let thread = self.verified_thread(lead).await?;
        if thread.model_provider.as_ref() != Some(&model_provider) {
            return Err(AppError::StaleRevision);
        }
        let mut page = self.turns(&thread.id, None, false, 1, false).await?;
        let turn = page.data.pop().ok_or(AppError::Unavailable)?;
        if turn.id != failed_turn.as_str() || turn.status != "failed" {
            return Err(AppError::StaleRevision);
        }
        if turn
            .error
            .as_ref()
            .and_then(|error| classify(error.codex_error_info.as_ref()?))
            != Some(kind)
        {
            return Err(AppError::StaleRevision);
        }
        let root_turn = turn
            .root_turn_id
            .as_deref()
            .ok_or(AppError::Unavailable)?
            .parse()
            .map_err(|_| AppError::Unavailable)?;
        if !self.connection.questions(&thread.id, None)?.is_empty() {
            return Err(AppError::Busy);
        }
        let WorkflowReadiness::Ready(ready) = self
            .workflow_readiness(lead, &thread, Some(&failed_turn))
            .await?
        else {
            return Err(AppError::Busy);
        };
        if ready.latest_user_turn_id.as_ref() != Some(&failed_turn)
            || ready.latest_user_entry_id.is_none()
        {
            return Err(AppError::Unavailable);
        }
        if thread
            .model
            .as_ref()
            .is_some_and(|current| current != &ready.model)
            || thread
                .reasoning_effort
                .as_ref()
                .is_some_and(|current| current != &ready.effort)
        {
            return Err(AppError::StaleRevision);
        }
        let usage = self.capacity(fresh).await?;
        if usage.account_hash().as_ref() != Some(&account) {
            return Err(AppError::StaleRevision);
        }
        let capacity = usage.allowance(&ready.model).ok_or(AppError::Busy)?;
        Ok((
            RecoveryAuthorization {
                episode,
                failed_turn,
                root_turn,
                kind,
                account,
                capacity,
                model_provider,
            },
            ready,
        ))
    }

    pub(super) async fn stop_exact_turn(
        &self,
        lead: LeadTarget,
        id: NativeTurnId,
    ) -> Result<InterruptState, AppError> {
        let thread = self.verified_thread(&lead).await?;
        if !matches!(thread.status, ThreadStatus::Active { .. }) {
            return Ok(InterruptState::Rejected);
        }
        let latest = self.turns(&thread.id, None, false, 1, false).await?;
        if latest
            .data
            .first()
            .is_none_or(|turn| turn.id != id.as_str() || turn.status != "inProgress")
        {
            return Ok(InterruptState::Rejected);
        }
        let _: Result<Value, _> = self
            .connection
            .read(
                "turn/interrupt",
                json!({"threadId":thread.id,"turnId":id.as_str()}),
            )
            .await;
        for delay in [0, 100, 250] {
            if delay != 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            let Ok(found) = self.turns(&thread.id, None, false, 1, false).await else {
                continue;
            };
            if found
                .data
                .first()
                .is_some_and(|turn| turn.id == id.as_str() && turn.status == "interrupted")
            {
                return Ok(InterruptState::Interrupted);
            }
        }
        Ok(InterruptState::Uncertain)
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Usage {
    account_id: Option<String>,
    ordinary_usage_allowed: Option<bool>,
    rate_limits: Limit,
    rate_limits_by_limit_id: Option<BTreeMap<String, Limit>>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Limit {
    limit_id: Option<String>,
    normal_model_slug: Option<String>,
    spend_control_reached: Option<bool>,
    primary: Option<Window>,
    secondary: Option<Window>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    used_percent: i32,
    resets_at: Option<i64>,
    window_duration_mins: Option<i64>,
}

impl Usage {
    pub(super) fn account_hash(&self) -> Option<ReportHash> {
        let id = self
            .account_id
            .as_deref()
            .filter(|id| !id.is_empty() && id.len() <= 256)?;
        Some(hash([b"codex-account", id.as_bytes()]))
    }

    pub(super) fn quota_episode(&self, failed: i64) -> Option<String> {
        let account = self.account_hash()?;
        let bucket = self
            .rate_limits
            .limit_id
            .as_deref()
            .filter(|id| !id.is_empty() && id.len() <= 128)?;
        let now = seconds()?;
        let mut exhausted = [
            self.rate_limits.primary.as_ref(),
            self.rate_limits.secondary.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|window| {
            let reset = window.resets_at?;
            let duration = window.window_duration_mins?.checked_mul(60)?;
            (window.used_percent >= 100
                && duration > 0
                && reset > now
                && failed < reset
                && failed >= reset.checked_sub(duration)?)
            .then_some(reset)
        });
        let reset = exhausted.next()?;
        if exhausted.next().is_some_and(|other| other != reset) {
            return None;
        }
        Some(format!(
            "codex-quota:{}",
            hash([
                account.as_str().as_bytes(),
                bucket.as_bytes(),
                &reset.to_be_bytes()
            ])
        ))
    }

    fn allowance(&self, model: &str) -> Option<ReportHash> {
        if self.ordinary_usage_allowed != Some(true) {
            return None;
        }
        let account = self.account_hash()?;
        let mut exact = self
            .rate_limits_by_limit_id
            .iter()
            .flat_map(|list| list.values())
            .filter(|row| row.normal_model_slug.as_deref() == Some(model));
        let selected = match exact.next() {
            Some(selected) if exact.next().is_none() => selected,
            Some(_) => return None,
            None if self.rate_limits.normal_model_slug.as_deref() == Some(model)
                || self.rate_limits.normal_model_slug.is_none()
                    && self.rate_limits.limit_id.as_deref() == Some("codex") =>
            {
                &self.rate_limits
            }
            None => return None,
        };
        let bucket = selected
            .limit_id
            .as_deref()
            .filter(|id| !id.is_empty() && id.len() <= 128)?;
        if selected.spend_control_reached != Some(false) {
            return None;
        }
        let now = seconds()?;
        let mut boundaries = Vec::new();
        for window in [selected.primary.as_ref(), selected.secondary.as_ref()]
            .into_iter()
            .flatten()
        {
            let reset = window.resets_at?;
            let duration = window.window_duration_mins?;
            if reset <= now || duration <= 0 || !(0..100).contains(&window.used_percent) {
                return None;
            }
            boundaries.extend_from_slice(&reset.to_be_bytes());
            boundaries.extend_from_slice(&duration.to_be_bytes());
        }
        if boundaries.is_empty() {
            return None;
        }
        Some(hash([
            account.as_str().as_bytes(),
            bucket.as_bytes(),
            boundaries.as_slice(),
        ]))
    }
}

fn hash<const N: usize>(values: [&[u8]; N]) -> ReportHash {
    let mut digest = Sha256::new();
    for value in values {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    ReportHash::from_sha256(digest.finalize().into())
}
fn seconds() -> Option<i64> {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}
