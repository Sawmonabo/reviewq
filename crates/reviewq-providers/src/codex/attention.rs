use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;

use reviewq_app::access::AppError;
use reviewq_app::hosts::recovery::{NativeFailure, RecoveryKind};
use reviewq_app::hosts::{HostActivity, HostPhase, QuestionStatus};
use reviewq_app::notifications::{Kind, NativeAttention, NativeOccurrence, TrackedNative};
use uuid::Uuid;

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
                // The writer is probed after the turn is read, so a process that opens the
                // thread in between holds it by the probe and the turn still counts as running.
                ThreadStatus::NotLoaded => {
                    let read = self.turns(&thread.id, None, false, 1, false).await;
                    let latest = read.ok().and_then(|page| page.data.into_iter().next());
                    activity = latest.as_ref().and_then(|turn| {
                        saved_turn_activity(turn, || {
                            writer_gone(self.profile.as_path(), &thread.id)
                        })
                    });
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
/// interrupted. A turn without an end time, which reads as interrupted, is running while a
/// process holds the thread, so the thread is unknown. Once none does, its process ended
/// without finishing it, as a killed or crashed one does, and the thread is idle.
fn saved_turn_activity(turn: &Turn, writer_gone: impl FnOnce() -> bool) -> Option<HostActivity> {
    let phase = match (turn.status.as_str(), turn.completed_at) {
        ("failed", _) => HostPhase::Failed,
        (_, Some(_)) => HostPhase::Idle,
        (_, None) if writer_gone() => HostPhase::Idle,
        (_, None) => return None,
    };
    Some(HostActivity {
        phase,
        has_questions: false,
    })
}

/// Whether no process holds the thread. A Codex process keeps an exclusive lock on
/// `thread-writer-locks/<thread>.lock` in its profile for every thread it has loaded, and the
/// system releases it when the process ends, however it ends. The probe first takes the
/// folder's coordination lock without waiting, as Codex does before it checks that a thread
/// is idle, so it never meets a process in the middle of taking a thread. A missing thread
/// lock means no process holds the thread. Anything else unclear counts as held.
fn writer_gone(profile: &Path, thread: &str) -> bool {
    let Ok(thread) = Uuid::parse_str(thread) else {
        return false;
    };
    let locks = profile.join("thread-writer-locks");
    let Ok(coordination) = File::open(locks.join(".coordination.lock")) else {
        return false;
    };
    if coordination.try_lock().is_err() {
        return false;
    }
    match File::open(locks.join(format!("{}.lock", thread.hyphenated()))) {
        Ok(lock) => lock.try_lock_shared().is_ok(),
        Err(error) => error.kind() == ErrorKind::NotFound,
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
