use std::fs::{self, File};
use std::io::Write;

use reviewq_app::workflow::{NativeCall, WorkflowTool};
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
    let mut calls = Vec::new();
    for (index, id) in ["exec-A", "exec-B"].into_iter().enumerate() {
        let (generation, position, nested) = codex_mcp_opening(
            binding.clone(),
            path.clone(),
            turn.clone(),
            id.to_owned(),
            Some("outer-item".to_owned()),
        )
        .await
        .unwrap();
        assert!(nested);
        calls.push(NativeCall {
            lead: lead.clone(),
            turn_id: turn.clone(),
            root_turn_id: None,
            call_id: id.to_owned(),
            source_generation: generation,
            source_position: position,
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
            "other".parse().unwrap(),
            "exec-A".into(),
            Some("outer-item".into())
        )
        .await
        .is_err()
    );
    assert!(
        codex_mcp_opening(binding, path.clone(), turn, "exec-A".into(), None)
            .await
            .is_err()
    );
    assert!(
        codex_report_terminal(calls[0].clone(), path.clone(), false)
            .await
            .unwrap()
            .is_none()
    );
    for row in [
        json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"thread","turn_id":"turn","item":{"type":"McpToolCall","id":"exec-B","server":"reviewq","tool":"report_handoff","status":"completed","arguments":{"handoff":{"outcome":"needs_input"}},"result":{"content":[{"type":"text","text":"ok"}]}}}}),
        json!({"type":"event_msg","payload":{"type":"task_complete","thread_id":"thread","turn_id":"turn"}}),
    ] {
        writeln!(file, "{row}").unwrap();
    }
    let first = codex_report_terminal(calls.remove(0), path.clone(), false)
        .await
        .unwrap()
        .unwrap();
    assert!(!first.result_seen);
    let second = codex_report_terminal(calls.remove(0), path, false)
        .await
        .unwrap()
        .unwrap();
    assert!(second.result_seen && second.normal);
}
