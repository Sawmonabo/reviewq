use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::time::Duration;

use reviewq_app::access::AppError;
use reviewq_app::hosts::recovery::{NativeFailure, RecoveryKind};
use reviewq_app::hosts::{HostActivity, HostPhase, NativeSessionMetadata, QuestionStatus, process};
use reviewq_app::notifications::{Kind, NativeAttention, NativeOccurrence, TrackedNative};
use serde::Deserialize;
use tokio::net::UnixStream;
use tokio::time::timeout;
use uuid::Uuid;

use super::Codex;
use super::connection::Reach;
use super::history::Turn;
use super::session::ThreadStatus;

/// How long after a daemon starts a thread it has not loaded may still be one it is restoring.
pub(super) const RESTORE_GRACE: Duration = Duration::from_secs(120);

impl Codex {
    pub(crate) async fn attention(
        &self,
        target: TrackedNative,
    ) -> Result<Option<NativeAttention>, AppError> {
        let thread = match self.bound_history_thread(&target.session).await {
            Ok(thread) => thread,
            Err(AppError::Unavailable) => return self.without_daemon(target).await,
            Err(error) => return Err(error),
        };
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
                    activity = match &latest {
                        Some(turn) => {
                            saved_turn_activity(turn, async {
                                self.daemon_settled() && self.thread_free(&thread.id).await
                            })
                            .await
                        }
                        None => None,
                    };
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

impl Codex {
    /// A session whose thread could not be read because no Codex daemon was reached. It is idle
    /// once four things hold together: no daemon start, restart, update or stop is under way,
    /// the daemon's socket is missing or refuses connections, no daemon process Codex launched
    /// is alive, and no process holds the thread. The lifecycle lock stays held from the first
    /// check to the last, so no command can launch a daemon in between. A daemon that a command
    /// restarts resumes the threads its predecessor saved, with any turn it interrupted, and
    /// takes them before it answers. The command holds the lock until the daemon answers or it
    /// gives up waiting; a daemon it gave up on is still alive, so threads saved while the lock
    /// is free and no daemon lives are ones nothing resumes. A socket that accepts the
    /// connection belongs to a daemon that runs, even one too busy to answer discovery in time.
    /// The observation carries no thread data, so the stored name, status and start time stay.
    /// Otherwise the read fails and nothing is stored.
    async fn without_daemon(
        &self,
        target: TrackedNative,
    ) -> Result<Option<NativeAttention>, AppError> {
        let thread = target.session.binding.identity().native_id().as_str();
        if matches!(self.connection.reach(), Reach::Bound(_)) {
            return Err(AppError::Unavailable);
        }
        let profile = self.profile.as_path().to_owned();
        let Some(lifecycle) = tokio::task::spawn_blocking(move || lifecycle_paused(&profile))
            .await
            .ok()
            .flatten()
        else {
            return Err(AppError::Unavailable);
        };
        let socket = self.profile.as_path().join(CONTROL_SOCKET);
        let no_listener = matches!(
            timeout(PROBE_LIMIT, UnixStream::connect(socket)).await,
            Ok(Err(error)) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused)
        );
        let (profile, owned) = (self.profile.as_path().to_owned(), thread.to_owned());
        let free = no_listener
            && tokio::task::spawn_blocking(move || {
                let free = !daemon_alive(&profile) && writer_gone(&profile, &owned);
                drop(lifecycle);
                free
            })
            .await
            .unwrap_or(false);
        if !free {
            return Err(AppError::Unavailable);
        }
        let async_questions = super::super::history::observe_async_questions(
            target.session,
            target.started_at_ms,
            target.questions,
        )
        .await
        .unwrap_or_default();
        Ok(Some(NativeAttention {
            activity: Some(HostActivity {
                phase: HostPhase::Idle,
                has_questions: false,
            }),
            metadata: NativeSessionMetadata::default(),
            children: None,
            terminal_failure: None,
            recoverable: None,
            events: Vec::new(),
            async_questions,
        }))
    }

    /// Whether the daemon that answered has run long enough to have restored the threads a
    /// previous daemon saved at shutdown. Until then, a thread it has not loaded may be one it
    /// is about to resume and continue.
    fn daemon_settled(&self) -> bool {
        matches!(self.connection.reach(), Reach::Bound(Some(bound))
            if bound.elapsed() >= self.restore_grace)
    }

    /// Whether no process holds the thread, probed off the async runtime.
    async fn thread_free(&self, thread: &str) -> bool {
        let (profile, thread) = (self.profile.as_path().to_owned(), thread.to_owned());
        tokio::task::spawn_blocking(move || writer_gone(&profile, &thread))
            .await
            .unwrap_or(false)
    }
}

/// The control socket Codex binds in its profile for the daemon it runs.
const CONTROL_SOCKET: &str = "app-server-control/app-server-control.sock";
/// The longest the lifecycle lock is held for a connection attempt to that socket.
const PROBE_LIMIT: Duration = Duration::from_secs(1);

/// A shared hold on a lock file, released explicitly so a child process forked meanwhile
/// cannot keep it held.
struct SharedLock(File);

impl Drop for SharedLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Holds Codex's daemon lifecycle lock shared, when no command holds it, so none can start,
/// restart, update or stop the daemon until the hold ends. Every such command takes
/// `app-server-daemon/daemon.lock` exclusively without waiting and retries while it is taken,
/// so a short hold only delays it. A missing lock file means no command has run in the
/// profile. `None` while a command runs or the lock cannot be read.
fn lifecycle_paused(profile: &Path) -> Option<Option<SharedLock>> {
    match File::open(profile.join("app-server-daemon/daemon.lock")) {
        Ok(file) => file.try_lock_shared().ok().map(|()| Some(SharedLock(file))),
        Err(error) if error.kind() == ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
}

/// Whether a daemon process Codex launched may still be alive, even one that has not bound
/// its socket yet. Codex records each daemon it launches in `app-server-daemon/daemon.pid`, or
/// `app-server.pid` for an older installation, with the boot and the start time in clock ticks
/// that identify the process, so a record left from an earlier boot or a process that ended
/// does not match. Codex records no pid namespace; it runs beside ReviewQ, so the pid is read
/// in ReviewQ's own. An empty or missing record names no process; anything unclear counts as
/// alive.
fn daemon_alive(profile: &Path) -> bool {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Record {
        pid: u32,
        /// Codex also reads this field under the name it gave it first.
        #[serde(alias = "linuxProcessIdentity")]
        process_identity: Option<Identity>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Identity {
        boot_id: String,
        start_ticks: u64,
    }
    let alive = |name: &str| {
        let mut text = String::new();
        match File::open(profile.join("app-server-daemon").join(name)) {
            Ok(file) => {
                if file.take(4096).read_to_string(&mut text).is_err() {
                    return true;
                }
            }
            Err(error) => return error.kind() != ErrorKind::NotFound,
        }
        if text.trim().is_empty() {
            return false;
        }
        let Ok(record) = serde_json::from_str::<Record>(&text) else {
            return true;
        };
        let ended = match record.process_identity {
            Some(identity) => {
                process::boot_id().is_some_and(|boot| boot != identity.boot_id)
                    || process::ended(record.pid, Some(identity.start_ticks))
            }
            None => process::ended(record.pid, None),
        };
        !ended
    };
    alive("daemon.pid") || alive("app-server.pid")
}

/// The activity of a thread this connection has not loaded, from its latest saved turn: failed
/// after a failed turn, and idle after any other turn with an end time, which completed or was
/// interrupted. A turn without an end time, which reads as interrupted, is running while a
/// process holds the thread, so the thread is unknown. Once none does, its process ended
/// without finishing it, as a killed or crashed one does, and the thread is idle. The writer
/// probe runs only for such a turn.
async fn saved_turn_activity(
    turn: &Turn,
    writer_gone: impl Future<Output = bool>,
) -> Option<HostActivity> {
    let phase = match (turn.status.as_str(), turn.completed_at) {
        ("failed", _) => HostPhase::Failed,
        (_, Some(_)) => HostPhase::Idle,
        (_, None) if writer_gone.await => HostPhase::Idle,
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
