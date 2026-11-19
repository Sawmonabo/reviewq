use reviewq_app::access::AppError;
use reviewq_app::hosts::{HostPhase, QuestionStatus};
use reviewq_app::notifications::{Kind, NativeAttention, NativeOccurrence, TrackedNative};
use reviewq_domain::identity::ReportHash;
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::Codex;
use super::session::{ThreadStatus, canonical_cwd};

impl Codex {
    pub(crate) async fn attention(
        &self,
        target: TrackedNative,
    ) -> Result<Option<NativeAttention>, AppError> {
        let binding = &target.session.binding;
        let identity = binding.identity();
        if identity.provider() != HostProvider::Codex || identity.profile() != &self.profile {
            return Err(AppError::NotFound);
        }
        let thread = self.read_thread(identity.native_id()).await?;
        if canonical_cwd(thread.cwd).await? != *binding.cwd() {
            return Err(AppError::Conflict);
        }
        let Some(mut activity) = thread.status.activity() else {
            return Ok(None);
        };
        let mut events = Vec::new();
        let mut terminal_failure = None;
        if let ThreadStatus::Active { active_flags } = &thread.status {
            for form in self.connection.questions(&thread.id, None)? {
                if form.state == QuestionStatus::Pending {
                    activity.has_questions = true;
                    events.push(occurrence(Kind::AgentQuestion, form.item.to_string(), None));
                }
            }
            if active_flags.iter().any(|flag| flag == "waitingOnApproval") {
                let page = self.turns(&thread.id, None, false, 1, false).await?;
                if let Some(turn) = page.data.first().filter(|turn| turn.status == "inProgress") {
                    events.push(occurrence(Kind::AgentPermission, turn.id.clone(), None));
                }
            }
        } else {
            let page = self.turns(&thread.id, None, false, 1, false).await?;
            if let Some(turn) = page.data.first().filter(|turn| turn.status == "failed") {
                let id = turn.id.parse().map_err(|_| AppError::Unavailable)?;
                if target.known_failure.as_ref() != Some(&id) {
                    let at = turn.completed_at.and_then(|stamp| stamp.checked_mul(1000));
                    let quota = turn
                        .error
                        .as_ref()
                        .and_then(|error| error.codex_error_info.as_ref())
                        .and_then(serde_json::Value::as_str)
                        == Some("usageLimitExceeded");
                    let kind = if quota {
                        Kind::Quota
                    } else {
                        Kind::AgentFailure
                    };
                    let mut event = occurrence(kind, turn.id.clone(), at);
                    if quota && let Some(seconds) = turn.completed_at {
                        event.desktop_episode = self.quota_episode(seconds).await;
                    }
                    events.push(event);
                }
                activity.phase = HostPhase::Failed;
                terminal_failure = Some(id);
            }
        }
        Ok(Some(NativeAttention {
            activity,
            terminal_failure,
            events,
        }))
    }

    async fn quota_episode(&self, failed: i64) -> Option<String> {
        let result: Usage = self
            .connection
            .read(
                "account/rateLimits/read",
                json!({"excludeResetCreditDetails":true}),
            )
            .await
            .ok()?;
        let account = result
            .account_id
            .filter(|id| !id.is_empty() && id.len() <= 256)?;
        let bucket = result
            .rate_limits
            .limit_id
            .filter(|id| !id.is_empty() && id.len() <= 128)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        let now = i64::try_from(now).ok()?;
        let mut exhausted = [result.rate_limits.primary, result.rate_limits.secondary]
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
        let mut hash = Sha256::new();
        hash.update((account.len() as u64).to_be_bytes());
        hash.update(account.as_bytes());
        hash.update((bucket.len() as u64).to_be_bytes());
        hash.update(bucket.as_bytes());
        hash.update(reset.to_be_bytes());
        Some(format!(
            "codex-quota:{}",
            ReportHash::from_sha256(hash.finalize().into())
        ))
    }
}

fn occurrence(kind: Kind, native_ref: String, at_ms: Option<i64>) -> NativeOccurrence {
    NativeOccurrence {
        kind,
        native_ref,
        at_ms,
        desktop_episode: None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    account_id: Option<String>,
    rate_limits: Limit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Limit {
    limit_id: Option<String>,
    primary: Option<Window>,
    secondary: Option<Window>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    used_percent: i32,
    resets_at: Option<i64>,
    window_duration_mins: Option<i64>,
}
