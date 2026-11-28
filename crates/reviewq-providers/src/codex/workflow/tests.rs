use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::HostPhase;
use reviewq_app::notifications::TrackedNative;
use reviewq_domain::session::{
    AbsolutePath, HostProvider, NativeBinding, NativeIdentity, SessionTarget,
};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

use super::{
    AppError, Codex, LeadTarget, NativeWorkflowProof, ThreadStatus, WorkflowHold,
    WorkflowReadiness, verify_resumed_followup,
};

#[tokio::test]
async fn only_followup_can_prepare_and_resume_the_exact_saved_lead() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().canonicalize().unwrap();
    let session_dir = profile.join("sessions");
    fs::create_dir(&session_dir).unwrap();
    let rollout = session_dir.join("rollout-thread.jsonl");
    let thread_id = "00000000-0000-4000-8000-000000000001";
    let turn_id = "saved-completed-turn";
    let tail = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\"}}\n".repeat(10_000);
    fs::write(&rollout, format!("{}\n{}\n{tail}",
        json!({"type":"session_meta","payload":{"id":thread_id,"cwd":profile}}),
        json!({"type":"turn_context","payload":{"turn_id":turn_id,"model":"gpt-test","effort":"high"}}),
    )).unwrap();
    let socket = profile.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let executable = profile.join("discover-codex");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\n",
            json!({
                "status":"running","socketPath":socket,"appServerVersion":"synthetic"
            })
        ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();

    let resumed = Arc::new(AtomicUsize::new(0));
    let recorded_resumes = resumed.clone();
    let turn_reads = Arc::new(AtomicUsize::new(0));
    let recorded_turn_reads = turn_reads.clone();
    let latest_turn = Arc::new(Mutex::new(("completed", Some(1))));
    let saved_turn = latest_turn.clone();
    let host_profile = profile.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let mut thread =
            json!({"id":thread_id,"cwd":host_profile,"path":rollout,"status":{"type":"notLoaded"}});
        while let Some(Ok(Message::Text(request))) = ws.next().await {
            let request: Value = serde_json::from_str(&request).unwrap();
            let id = &request["id"];
            let params = &request["params"];
            let data = match request["method"].as_str().unwrap() {
                "initialize" => json!({"codexHome":host_profile}),
                "initialized" => continue,
                "thread/read" => {
                    assert_eq!(*params, json!({"threadId":thread_id,"includeTurns":false}));
                    json!({"thread":thread})
                }
                "thread/turns/list" => {
                    assert_eq!(params["threadId"], thread_id);
                    assert_eq!(params["limit"], 1);
                    recorded_turn_reads.fetch_add(1, Ordering::Relaxed);
                    let (status, completed_at) = *saved_turn.lock().unwrap();
                    json!({"data":[{"id":turn_id,"status":status,"completedAt":completed_at,"items":[
                        {"type":"userMessage","id":"last-request","content":[]}
                    ]}]})
                }
                "thread/list" => json!({"data":[],"nextCursor":null}),
                "thread/resume" => {
                    assert_eq!(*params, json!({"threadId":thread_id,"excludeTurns":true}));
                    let mut response = thread.clone();
                    response["status"] = json!({"type":"idle"});
                    match recorded_resumes.fetch_add(1, Ordering::Relaxed) {
                        0 => response["path"] = json!(host_profile.join("other.jsonl")),
                        1 => {
                            response["status"] =
                                json!({"type":"active","activeFlags":["waitingOnApproval"]})
                        }
                        _ => thread = response.clone(),
                    }
                    json!({"thread":response})
                }
                other => panic!("readiness and resume requested unexpected method {other}"),
            };
            ws.send(Message::Text(
                json!({"id":id,"result":data}).to_string().into(),
            ))
            .await
            .unwrap();
        }
    });
    let mut codex = Codex::new(executable.clone(), profile.clone(), None).unwrap();
    // The synthetic daemon has just started; it restores nothing.
    codex.restore_grace = std::time::Duration::ZERO;
    let codex = Arc::new(codex);
    let readers = crate::workflow::Workflows::new(Some(codex.clone()));
    let profile = AbsolutePath::from_canonical(profile).unwrap();
    let lead = LeadTarget::new(
        SessionTarget::new(
            "review".parse().unwrap(),
            "run".parse().unwrap(),
            "session".parse().unwrap(),
        ),
        NativeBinding::new(
            NativeIdentity::new(
                HostProvider::Codex,
                profile.clone(),
                thread_id.parse().unwrap(),
            ),
            profile,
        ),
    );

    // The thread is loaded only in another process. Its latest saved turn shows it idle once
    // the turn has an end time, completed or interrupted. Without one, which is how a turn
    // running elsewhere reads, it is unknown while a process holds the thread's writer lock or
    // the lock cannot be read, and idle once no process holds it, as after a killed process.
    // Each observation of a lead that settling waits on reads one turn; any other session's
    // reads none.
    let tracked = |idle_decides| TrackedNative {
        session: HistorySession {
            target: lead.target().clone(),
            binding: lead.binding().clone(),
            source: None,
            parent: None,
        },
        known_failure: None,
        child_cursor: None,
        started_at_ms: 0,
        questions: Vec::new(),
        idle_decides,
    };
    let phase = async |idle_decides| {
        let observed = codex
            .attention(tracked(idle_decides))
            .await
            .unwrap()
            .unwrap();
        observed.activity.map(|seen| seen.phase)
    };
    assert_eq!(phase(true).await, Some(HostPhase::Idle));
    *latest_turn.lock().unwrap() = ("interrupted", None);
    assert_eq!(phase(true).await, None);
    let locks = session_dir.with_file_name("thread-writer-locks");
    fs::create_dir(&locks).unwrap();
    fs::write(locks.join(".coordination.lock"), "").unwrap();
    let writer = fs::File::create(locks.join(format!("{thread_id}.lock"))).unwrap();
    writer.lock().unwrap();
    assert_eq!(phase(true).await, None);
    drop(writer);
    assert_eq!(phase(true).await, Some(HostPhase::Idle));
    *latest_turn.lock().unwrap() = ("interrupted", Some(2));
    assert_eq!(phase(true).await, Some(HostPhase::Idle));
    assert_eq!(phase(false).await, None);
    assert_eq!(turn_reads.load(Ordering::Relaxed), 5);
    *latest_turn.lock().unwrap() = ("completed", Some(1));

    assert!(matches!(
        readers.dispatch_readiness(lead.clone()).await.unwrap(),
        WorkflowReadiness::Held(WorkflowHold::SessionNotLoaded)
    ));
    let WorkflowReadiness::Ready(expected) =
        readers.followup_readiness(lead.clone()).await.unwrap()
    else {
        panic!("saved followup is ready without a resume");
    };
    assert_eq!(expected.model, "gpt-test");
    assert_eq!(expected.effort, "high");
    assert_eq!(
        expected.latest_user_turn_id.as_ref().unwrap().as_str(),
        turn_id
    );
    assert_eq!(resumed.load(Ordering::Relaxed), 0);
    let saved = codex.verified_thread(&lead).await.unwrap();
    let mut changed = expected.clone();
    changed.effort = "low".to_owned();
    assert_eq!(
        codex
            .verify_saved_followup(&lead, &saved, &changed)
            .await
            .err(),
        Some(AppError::StaleRevision)
    );
    assert_eq!(resumed.load(Ordering::Relaxed), 0);

    codex
        .verify_saved_followup(&lead, &saved, &expected)
        .await
        .unwrap();
    for _ in 0..2 {
        let unproved = codex
            .resume_exact_thread(&lead, &saved, None)
            .await
            .unwrap();
        assert_eq!(
            verify_resumed_followup(&saved, &unproved).err(),
            Some(AppError::Conflict)
        );
    }
    let loaded = codex
        .resume_exact_thread(&lead, &saved, None)
        .await
        .unwrap();
    verify_resumed_followup(&saved, &loaded).unwrap();
    assert!(matches!(loaded.status, ThreadStatus::Idle));
    assert!(
        matches!(codex.idle_readiness(&lead, &loaded).await.unwrap(), WorkflowReadiness::Ready(ready) if ready == expected)
    );
    assert_eq!(resumed.load(Ordering::Relaxed), 3);
    server.abort();
    let _ = server.await;

    // Discovery now fails, as it does when its 2-second probe gets no answer. The session is
    // idle only while no daemon command runs, no daemon accepts a connection on the profile's
    // control socket and no process holds the thread. Threads a daemon saved at shutdown do not
    // count once no command that restarts it runs.
    fs::write(&executable, "#!/bin/sh\nexit 1\n").unwrap();
    // The first observation may still meet the closed connection.
    let _ = codex.attention(tracked(true)).await;
    let unknown =
        async || codex.attention(tracked(true)).await.err() == Some(AppError::Unavailable);
    let control = session_dir.with_file_name("app-server-control");
    fs::create_dir(&control).unwrap();
    let listening = UnixListener::bind(control.join("app-server-control.sock")).unwrap();
    assert!(unknown().await);
    drop(listening);
    let state = session_dir.with_file_name("app-server-daemon");
    fs::create_dir(&state).unwrap();
    fs::write(
        state.join("loaded-threads.json"),
        json!({"loaded":[thread_id]}).to_string(),
    )
    .unwrap();
    assert_eq!(phase(true).await, Some(HostPhase::Idle));
    let command = fs::File::create(state.join("daemon.lock")).unwrap();
    command.lock().unwrap();
    assert!(unknown().await);
    drop(command);
    assert_eq!(phase(true).await, Some(HostPhase::Idle));

    // A daemon a command launched but gave up waiting for may still resume the saved threads,
    // so its live process keeps the session unknown. A record from an earlier boot does not.
    let ticks = reviewq_app::hosts::process::start_time(std::process::id()).unwrap();
    let boot = reviewq_app::hosts::process::boot_id().unwrap();
    let record = |boot: &str| {
        json!({"pid":std::process::id(),"processStartTime":"",
            "processIdentity":{"bootId":boot,"startTicks":ticks}})
        .to_string()
    };
    fs::write(state.join("daemon.pid"), record(&boot)).unwrap();
    assert!(unknown().await);
    fs::write(state.join("daemon.pid"), record("an-earlier-boot")).unwrap();
    assert_eq!(phase(true).await, Some(HostPhase::Idle));
}
