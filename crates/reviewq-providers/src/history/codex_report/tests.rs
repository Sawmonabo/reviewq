use std::fs::{self, File};
use std::io::Write;

use reviewq_app::workflow::{ExpectedAcknowledgement, NativeCall, WorkflowTool};
use reviewq_domain::session::{
    AbsolutePath, HostProvider, LeadTarget, NativeBinding, NativeIdentity, SessionTarget,
};
use serde_json::json;

use super::codex_report_terminal;
use crate::history::codex_mcp_opening;

#[tokio::test]
async fn parallel_mcp_calls_in_one_code_cell_require_their_own_native_result() {
    let profile = tempfile::tempdir().unwrap();
    let root = AbsolutePath::from_canonical(profile.path().to_owned()).unwrap();
    let binding = NativeBinding::new(
        NativeIdentity::new(HostProvider::Codex, root.clone(), "thread".parse().unwrap()),
        root,
    );
    let lead = LeadTarget::new(
        SessionTarget::new(
            "review".parse().unwrap(),
            "run".parse().unwrap(),
            "session".parse().unwrap(),
        ),
        binding.clone(),
    );
    let dir = profile.path().join("sessions");
    fs::create_dir(&dir).unwrap();
    let path = dir.join("rollout-thread.jsonl");
    let mut file = File::create(&path).unwrap();
    for row in [
        json!({"type":"session_meta","payload":{"id":"thread","cwd":profile.path()}}),
        json!({"type":"event_msg","payload":{"type":"task_started","thread_id":"thread","turn_id":"turn"}}),
        json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"outer-item","call_id":"outer","name":"exec","input":"await Promise.all(calls)"}}),
    ] {
        writeln!(file, "{row}").unwrap();
    }
    let turn: reviewq_domain::identity::NativeTurnId = "turn".parse().unwrap();
    let arguments = json!({"handoff": {"outcome": "needs_input","reason":"fixture"}});
    let digest = "f".repeat(64);
    let expected = ExpectedAcknowledgement {
        receipt_id: "receipt".parse().unwrap(),
        declaration_sha256: digest.parse().unwrap(),
    };
    let ack =
        json!({"receiptId":"receipt","declarationSha256":digest,"state":"pending_completion"})
            .to_string();
    let mut calls = Vec::new();
    for (index, id) in ["exec-A", "exec-B"].into_iter().enumerate() {
        let opening = codex_mcp_opening(
            binding.clone(),
            path.clone(),
            id.to_owned(),
            Some("outer-item".to_owned()),
            json!({}),
            None,
        )
        .await
        .unwrap();
        assert!(opening.nested);
        calls.push(NativeCall {
            lead: lead.clone(),
            turn_id: turn.clone(),
            root_turn_id: None,
            call_id: id.to_owned(),
            source_generation: opening.generation,
            source_position: opening.position,
            arguments_sha256: Some(
                crate::history::native_arguments_hash(arguments.clone()).unwrap(),
            ),
            source_ordinal: index as u32 + 1,
            request_entry_id: None,
            tool: WorkflowTool::Report,
        });
    }
    assert_eq!(calls[0].source_position, calls[1].source_position);
    assert!(
        codex_mcp_opening(
            binding.clone(),
            path.clone(),
            "exec-A".into(),
            Some("outer-item".into()),
            json!({}),
            Some("wrong-turn".parse().unwrap())
        )
        .await
        .is_err()
    );
    assert!(
        codex_mcp_opening(
            binding,
            path.clone(),
            "exec-A".into(),
            None,
            json!({}),
            None
        )
        .await
        .is_err()
    );
    assert!(
        codex_report_terminal(
            calls[0].clone(),
            path.clone(),
            false,
            Some(expected.clone())
        )
        .await
        .unwrap()
        .is_none()
    );
    for row in [
        json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"thread","turn_id":"turn","item":{"type":"McpToolCall","id":"exec-B","server":"reviewq","tool":"report_handoff","status":"completed","arguments":arguments,"result":{"content":[{"type":"text","text":ack}]}}}}),
        json!({"type":"event_msg","payload":{"type":"task_complete","thread_id":"thread","turn_id":"turn"}}),
    ] {
        writeln!(file, "{row}").unwrap();
    }
    let first = codex_report_terminal(calls.remove(0), path.clone(), false, Some(expected.clone()))
        .await
        .unwrap()
        .unwrap();
    assert!(!first.result_seen);
    let mut altered = calls[0].clone();
    altered.arguments_sha256 =
        Some(crate::history::native_arguments_hash(json!({"unexpected":true})).unwrap());
    assert!(
        codex_report_terminal(altered, path.clone(), false, Some(expected.clone()))
            .await
            .is_err()
    );
    let second = codex_report_terminal(calls.remove(0), path, false, Some(expected))
        .await
        .unwrap()
        .unwrap();
    assert!(second.result_seen && second.normal);
}
