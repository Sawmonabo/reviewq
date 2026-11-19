use reviewq_app::access::AppError;
use reviewq_app::hosts::recovery::{NativeFailure, RecoveryKind};
use reviewq_app::hosts::{HostPhase, QuestionStatus};
use reviewq_app::notifications::{Kind, NativeAttention, NativeOccurrence, TrackedNative};

use super::Codex;
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
        } else if let Some(activity) = &mut activity {
            let page = self.turns(&thread.id, None, false, 1, false).await?;
            if let Some(turn) = page.data.first().filter(|turn| turn.status == "failed") {
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

fn occurrence(kind: Kind, native_ref: String, at_ms: Option<i64>) -> NativeOccurrence {
    NativeOccurrence {
        kind,
        native_ref,
        at_ms,
        desktop_episode: None,
    }
}
