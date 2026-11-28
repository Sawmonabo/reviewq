use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio_tungstenite::{accept_async, tungstenite::Message};

use super::{Connection, DEADLINE, Effect, home_owner};

/// A guard whose check is slow, as a store read waiting its turn is, no longer stops the
/// loop: calls queued after it are written and answered meanwhile, and it is written once
/// its check passes, even after longer than a reply may take. The synthetic daemon lives in a private folder; no Codex runs.
#[tokio::test]
async fn a_slow_guard_check_leaves_the_session_serving_other_calls() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().canonicalize().unwrap();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = profile.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let executable = profile.join("discover-codex");
    let discovery = json!({"status":"running","socketPath":socket,"appServerVersion":"synthetic"});
    fs::write(
        &executable,
        format!("#!/bin/sh\nprintf '%s\\n' '{discovery}'\n"),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();

    let written = Arc::new(Mutex::new(Vec::new()));
    let seen = written.clone();
    let home = profile.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(request))) = ws.next().await {
            let request: Value = serde_json::from_str(&request).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "initialize" => json!({"codexHome":home}),
                "initialized" => continue,
                method => {
                    seen.lock().unwrap().push(method.to_owned());
                    json!({})
                }
            };
            let reply = json!({"id":request["id"],"result":result});
            ws.send(Message::Text(reply.to_string().into()))
                .await
                .unwrap();
        }
    });

    let connection = Connection::new(executable, profile.clone(), home_owner(&profile).unwrap());
    let (release, held) = oneshot::channel();
    let guarded = connection.call("guarded", json!({}), Some(Effect::Wait(held)));
    let others = async {
        for _ in 0..3 {
            assert!(connection.call("plain", json!({}), None).await.is_ok());
        }
        // A check may outlast a written request's own wait for its reply.
        tokio::time::sleep(DEADLINE + Duration::from_secs(1)).await;
        release.send(()).unwrap();
    };
    let (guarded, ()) = tokio::time::timeout(Duration::from_secs(12), async {
        tokio::join!(guarded, others)
    })
    .await
    .expect("calls behind a slow guard check were answered");
    assert!(guarded.is_ok());
    assert_eq!(
        *written.lock().unwrap(),
        ["plain", "plain", "plain", "guarded"]
    );
}
