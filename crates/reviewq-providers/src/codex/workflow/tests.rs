use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::{SinkExt, StreamExt};
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
                    json!({"data":[{"id":turn_id,"status":"completed","items":[
                        {"type":"userMessage","id":"last-request","content":[]}
                    ]}]})
                }
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
    let codex = Arc::new(Codex::new(executable, profile.clone(), None).unwrap());
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
}
