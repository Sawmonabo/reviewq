use std::fs::{self, OpenOptions};
use std::io::Write;

use reviewq_app::history::{
    HistorySession, NativeHistoryReader, NativePosition, ToolState, VisiblePartKind,
    search::HistorySearch,
};
use reviewq_app::reading::Literal;
use reviewq_domain::session::{
    AbsolutePath, HostProvider, NativeBinding, NativeIdentity, SessionTarget,
};
use serde_json::{Value, json};

use crate::history::FileHistory;

fn invocation(id: &str, namespace: &str) -> Value {
    json!({"type":"response_item", "payload":{
        "type":"function_call", "name":"wait_agent", "namespace":namespace, "call_id":id,
        "arguments":json!({"timeout_ms":5000, "extra":"hidden input"}).to_string(),
        "internal_chat_message_metadata_passthrough":{"turn_id":"turn"},
    }})
}

fn completion(id: &str, sender: &str) -> Value {
    json!({"type":"event_msg", "payload":{
        "type":"item_completed", "thread_id":"thread", "turn_id":"turn", "item":{
            "type":"CollabAgentToolCall", "id":id, "sender_thread_id":sender,
            "tool":"wait", "status":"completed", "output":"unverified event output",
        },
    }})
}

fn output(id: &str, turn: &str) -> Value {
    json!({"type":"response_item", "payload":{
        "type":"function_call_output", "call_id":id,
        "output":json!({"message":"Wait timed out.","timed_out":true,"extra":"hidden output"}).to_string(),
        "internal_chat_message_metadata_passthrough":{"turn_id":turn},
    }})
}

#[tokio::test]
async fn wait_details_require_exact_records_and_follow_the_existing_content_locator() {
    let profile = tempfile::tempdir().unwrap();
    let directory = profile.path().join("sessions");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("rollout-thread.jsonl");
    let profile_root = AbsolutePath::from_canonical(profile.path().to_path_buf()).unwrap();
    let session = HistorySession {
        target: SessionTarget::new(
            "review".parse().unwrap(),
            "run".parse().unwrap(),
            "session".parse().unwrap(),
        ),
        binding: NativeBinding::new(
            NativeIdentity::new(
                HostProvider::Codex,
                profile_root.clone(),
                "thread".parse().unwrap(),
            ),
            profile_root,
        ),
        source: Some(AbsolutePath::from_canonical(path.clone()).unwrap()),
        parent: None,
    };
    let mut file = OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(path)
        .unwrap();
    for row in [
        json!({"type":"session_meta","payload":{"id":"thread","cwd":profile.path()}}),
        invocation("wrong-sender", "collaboration"),
        completion("wrong-sender", "other-thread"),
        output("wrong-sender", "turn"),
        invocation("wrong-namespace", "other-tool"),
        completion("wrong-namespace", "thread"),
        output("wrong-namespace", "turn"),
        invocation("wrong-turn", "collaboration"),
        completion("wrong-turn", "thread"),
        output("wrong-turn", "other-turn"),
        invocation("valid", "collaboration"),
        json!({"type":"token_usage_record","payload":{"thread_id":"thread","turn_id":"turn"}}),
        completion("valid", "thread"),
    ] {
        writeln!(file, "{row}").unwrap();
    }
    write!(file, "{}", output("valid", "turn")).unwrap();

    let reader = FileHistory::default();
    let page = reader
        .page(session.clone(), NativePosition::default(), 10)
        .await
        .unwrap()
        .unwrap();
    let [sender, namespace, wrong_turn, pending] = page.entries.as_slice() else {
        panic!("one entry per completed item");
    };
    assert!(sender.parts[0].text.is_empty());
    assert!(namespace.parts[0].text.is_empty());
    for entry in [sender, namespace, wrong_turn, pending] {
        assert_eq!(entry.parts.len(), 1);
    }
    assert!(matches!(
        pending.parts[0].kind,
        VisiblePartKind::ToolCall {
            label: "AgentWait",
            state: ToolState::Completed,
            ..
        }
    ));
    assert_eq!(pending.parts[0].text, r#"{"timeout_ms":5000}"#);

    writeln!(file).unwrap();
    let page = reader
        .page(session.clone(), NativePosition::default(), 10)
        .await
        .unwrap()
        .unwrap();
    let valid = page.entries.last().unwrap();
    assert_eq!(valid.id.as_str(), "valid");
    let result = r#"{"message":"Wait timed out.","timed_out":true}"#;
    assert_eq!(valid.parts[1].text, result);
    let reference = valid.content.as_ref().unwrap().file().unwrap().clone();
    let tail = reader.parts(session.clone(), reference, 1).await.unwrap();
    assert_eq!(tail.parts.len(), 1);
    assert_eq!(tail.parts[0].text, result);

    let search = reader
        .search(
            session.clone(),
            HistorySearch {
                literal: Literal::new("Wait timed out.").unwrap(),
                from: None,
                reverse: false,
            },
        )
        .await
        .unwrap()
        .unwrap();
    let [hit] = search.hits.as_slice() else {
        panic!("only the verified result matches")
    };
    assert_eq!(hit.reference.entry, valid.id);
    assert_eq!(hit.tool_call.as_ref(), Some(&valid.id));
    let chunk = reader
        .content(session, hit.reference.clone(), 0, 64)
        .await
        .unwrap();
    assert_eq!(chunk.text, result);
    assert_eq!(chunk.next_offset, None);
}
