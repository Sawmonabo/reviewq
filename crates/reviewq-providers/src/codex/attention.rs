use reviewq_app::access::AppError;
use reviewq_app::hosts::recovery::{NativeFailure, RecoveryKind};
use reviewq_app::hosts::{HostActivity, HostPhase, QuestionStatus};
use reviewq_app::notifications::{Kind, NativeAttention, NativeOccurrence, TrackedNative};

use super::Codex;
use super::history::Turn;
use super::session::ThreadStatus;

impl Codex {
    pub(crate) async fn attention(
        &self,
        target: TrackedNative,
    ) -> Result<Option<NativeAttention>, AppError> {
        let thread = self.bound_history_thread(&target.session).await?;
        let mut activity = thread.status.activity();
        let mut metadata = thread.metadata();
        let mut events = Vec::new();
        let mut terminal_failure = None;
        let mut recoverable = None;
        if let (Some(activity), ThreadStatus::Active { active_flags }) =
            (&mut activity, &thread.status)
        {
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
            let latest = match thread.status {
                // Only a lead whose idleness settling waits on is worth reading the saved turns
                // for. Without a readable turn the thread is unknown; its stored state is
                // replaced, not kept, because another process may have started a turn since.
                ThreadStatus::NotLoaded if !target.idle_decides => None,
                ThreadStatus::NotLoaded => {
                    let read = self.turns(&thread.id, None, false, 1, false).await;
                    let latest = read.ok().and_then(|page| page.data.into_iter().next());
                    activity = latest.as_ref().and_then(saved_turn_activity);
                    latest
                }
                _ => self
                    .turns(&thread.id, None, false, 1, false)
                    .await?
                    .data
                    .into_iter()
                    .next(),
            };
            if let Some(activity) = &mut activity
                && let Some(turn) = latest.filter(|turn| turn.status == "failed")
            {
                let id = turn.id.parse().map_err(|_| AppError::Unavailable)?;
                if target.known_failure.as_ref() != Some(&id) {
                    let at = turn.completed_at.and_then(|stamp| stamp.checked_mul(1000));
                    let failure = turn.error.as_ref().and_then(|error| {
                        super::recovery::classify(error.codex_error_info.as_ref()?)
                    });
                    let quota = failure == Some(RecoveryKind::UsageLimit);
                    let mut event = occurrence(
                        if quota {
                            Kind::Quota
                        } else {
                            Kind::AgentFailure
                        },
                        turn.id.clone(),
                        at,
                    );
                    if let Some(kind) = failure {
                        let usage = self.capacity(false).await.ok();
                        if quota
                            && let (Some(usage), Some(seconds)) =
                                (usage.as_ref(), turn.completed_at)
                        {
                            event.desktop_episode = usage.quota_episode(seconds);
                        }
                        recoverable = Some(NativeFailure {
                            turn: id.clone(),
                            kind,
                            account: usage.as_ref().and_then(|usage| usage.account_hash()),
                            model_provider: thread.model_provider.clone(),
                            at_ms: at,
                        });
                    }
                    events.push(event);
                }
                activity.phase = HostPhase::Failed;
                metadata.status = Some("failed".to_owned());
                terminal_failure = Some(id);
            }
        }
        let children = self
            .children(&target.session, target.child_cursor.as_ref())
            .await
            .ok();
        let async_questions = super::super::history::observe_async_questions(
            target.session,
            target.started_at_ms,
            target.questions,
        )
        .await
        .unwrap_or_default();
        Ok(Some(NativeAttention {
            activity,
            metadata,
            children,
            terminal_failure,
            recoverable,
            events,
            async_questions,
        }))
    }
}

/// The activity of a thread this connection has not loaded, from its latest saved turn: failed
/// after a failed turn, and idle after any other turn with an end time, which completed or was
/// interrupted. A turn without an end time is still running in another process, which reports
/// it as interrupted, so the thread is unknown.
fn saved_turn_activity(turn: &Turn) -> Option<HostActivity> {
    let phase = match (turn.status.as_str(), turn.completed_at) {
        ("failed", _) => HostPhase::Failed,
        (_, Some(_)) => HostPhase::Idle,
        (_, None) => return None,
    };
    Some(HostActivity {
        phase,
        has_questions: false,
    })
}

fn occurrence(kind: Kind, native_ref: String, at_ms: Option<i64>) -> NativeOccurrence {
    NativeOccurrence {
        kind,
        native_ref,
        at_ms,
        desktop_episode: None,
    }
}
