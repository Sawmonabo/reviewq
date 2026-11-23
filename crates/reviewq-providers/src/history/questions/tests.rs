use std::fs::{self, OpenOptions};
use std::io::Write;

use reviewq_app::history::{HistorySession, NativeHistoryReader, VisiblePartKind};
use reviewq_domain::session::{AbsolutePath, NativeBinding, NativeIdentity, SessionTarget};
use serde_json::{Value, json};

use super::*;
use crate::history::{FileHistory, locator, observe_async_questions};

fn event(thread: &str, turn: &str, item: Value) -> String {
    format!(
        "{}\n",
        json!({
            "type":"event_msg", "timestamp":"2026-11-21T22:50:05Z",
            "payload":{"type":"item_completed","thread_id":thread,"turn_id":turn,"item":item}
        })
    )
}

fn reply(thread: &str) -> String {
    event(
        thread,
        "turn-response",
        json!({
            "type":"UserMessage","id":"item-response",
            "content":[{"type":"text","text":"Answer to Codex question (turn-question:call_question):\n\nquestion-1: Keep both"}]
        }),
    )
}

fn fixture(path: &std::path::Path, prose: &str) -> (HistorySession, String) {
    let directory = path.join("sessions");
    fs::create_dir(&directory).unwrap();
    let rollout = directory.join("rollout-thread.jsonl");
    let open = event(
        "thread",
        "turn-question",
        json!({
            "type":"AgentMessage","id":"call_question","delivery":"async","text":prose,
            "questions":[
                {"title":"Which records should be kept?", "options":["Keep both","Keep first"]},
                {"title":"Private question", "is_secret":true}
            ]
        }),
    );
    let bytes = format!(
        "{}\n{open}",
        json!({"type":"session_meta","payload":{"id":"thread","cwd":path}})
    );
    let path = AbsolutePath::from_canonical(path.to_path_buf()).unwrap();
    (
        HistorySession {
            target: SessionTarget::new(
                "review".parse().unwrap(),
                "run".parse().unwrap(),
                "session".parse().unwrap(),
            ),
            binding: NativeBinding::new(
                NativeIdentity::new(HostProvider::Codex, path.clone(), "thread".parse().unwrap()),
                path,
            ),
            source: Some(AbsolutePath::from_canonical(rollout).unwrap()),
            parent: None,
        },
        bytes,
    )
}

#[tokio::test]
async fn answered_details_and_jump_survive_copy_to_new_generation() {
    let profile = tempfile::tempdir().unwrap();
    let (session, bytes) = fixture(profile.path(), "Preserve this message.");
    let bytes = bytes + &reply("thread");
    let rollout = session.source.as_ref().unwrap().as_path();
    fs::write(rollout, &bytes).unwrap();
    let known = observe_async_questions(session.clone(), 0, Vec::new())
        .await
        .unwrap();
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].state, QuestionStatus::Answered);

    // Keep the original inode allocated so the replacement cannot reuse it.
    fs::rename(rollout, rollout.with_extension("original")).unwrap();
    fs::write(rollout, &bytes).unwrap();
    let reader = FileHistory::default();
    let forms = reader
        .async_questions(session.clone(), known.clone(), None)
        .await
        .unwrap();
    let [form] = forms.as_slice() else {
        panic!("one saved form")
    };
    assert_eq!(form.state, QuestionStatus::Answered);
    assert_eq!(form.questions[0].options[0].label, "Keep both");

    let locator = form.locator.clone().unwrap();
    assert_ne!(locator.generation, known[0].key.anchor.generation);
    let around = reader
        .around(session.clone(), form.item.clone(), Some(locator), 2)
        .await
        .unwrap()
        .unwrap();
    let [opening, reply] = around.entries.as_slice() else {
        panic!("opening and reply")
    };
    assert_eq!(opening.turn.as_ref(), Some(&form.turn));
    let [prose, question] = opening.parts.as_slice() else {
        panic!("one message and one question")
    };
    assert_eq!(prose.text, "Preserve this message.");
    let VisiblePartKind::UserQuestion { request, questions } = &question.kind else {
        panic!("question in its native position")
    };
    assert_eq!(request, &known[0].key.request);
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].prompt, "Which records should be kept?");
    assert_eq!(reply.id.as_str(), "item-response");
    assert!(reply.text.ends_with("question-1: Keep both"));

    for altered in [Some(bytes.replace("Keep both", "Keep none")), None] {
        match altered {
            Some(altered) => fs::write(rollout, altered).unwrap(),
            None => fs::remove_file(rollout).unwrap(),
        }
        let forms = reader
            .async_questions(session.clone(), known.clone(), None)
            .await
            .unwrap();
        assert_eq!(forms[0].state, QuestionStatus::Answered);
        assert!(forms[0].questions.is_empty());
        assert!(forms[0].locator.is_none());
    }
}

#[tokio::test]
async fn pending_send_proof_requires_current_generation_and_complete_unanswered_source() {
    let profile = tempfile::tempdir().unwrap();
    let (session, bytes) = fixture(profile.path(), "");
    let rollout = session.source.as_ref().unwrap().as_path();
    fs::write(rollout, bytes).unwrap();
    let known = observe_async_questions(session.clone(), 0, Vec::new())
        .await
        .unwrap();
    let pending = &known[0];
    let cancelled = AtomicBool::new(false);
    let source = locator::locate(&session, &cancelled).unwrap().unwrap();
    assert_eq!(
        prove(&source, pending, &cancelled).unwrap().state,
        QuestionStatus::Pending
    );

    let mut relocated = pending.clone();
    relocated.key.anchor.generation = "different".to_owned();
    assert_eq!(
        prove(&source, &relocated, &cancelled).err(),
        Some(AppError::StaleRevision)
    );
    let forms = read(&source, &[relocated], None, &cancelled).unwrap();
    assert_eq!(forms[0].state, QuestionStatus::Unavailable);
    assert!(!forms[0].questions.is_empty());

    let mut append = OpenOptions::new().append(true).open(rollout).unwrap();
    append.write_all(reply("other-thread").as_bytes()).unwrap();
    assert_eq!(
        prove(&source, pending, &cancelled).unwrap().state,
        QuestionStatus::Pending
    );
    append.write_all(reply("thread").as_bytes()).unwrap();
    assert_eq!(
        prove(&source, pending, &cancelled).unwrap().state,
        QuestionStatus::Answered
    );
    let forms = read(&source, &known, Some(&pending.key.request), &cancelled).unwrap();
    assert_eq!(forms[0].state, QuestionStatus::Answered);
    append.write_all(b"{").unwrap();
    assert_eq!(
        prove(&source, pending, &cancelled).err(),
        Some(AppError::Unavailable)
    );
}

#[tokio::test]
async fn native_opening_is_located_without_showing_tool_payloads() {
    let profile = tempfile::tempdir().unwrap();
    let (session, mut bytes) = fixture(profile.path(), "");
    let turn = "native-turn";
    let id = "native-call";
    for record in [
        json!({"type":"turn_context","payload":{"turn_id":turn}}),
        json!({"type":"response_item","timestamp":"2026-11-21T22:50:05Z","payload":{
            "type":"function_call","call_id":id,"name":"request_user_input",
            "arguments":"private native tool argument"
        }}),
        json!({"type":"response_item","payload":{
            "type":"function_call_output","call_id":id,"output":"private native result"
        }}),
    ] {
        bytes.push_str(&format!("{record}\n"));
    }
    let rollout = session.source.as_ref().unwrap().as_path();
    fs::write(rollout, bytes).unwrap();
    let cancel = AtomicBool::new(false);
    let source = locator::locate(&session, &cancel).unwrap().unwrap();
    let target: (NativeTurnId, NativeInputId) = (turn.parse().unwrap(), id.parse().unwrap());
    let wrong_turn = ("other-turn".parse().unwrap(), target.1.clone());
    let found = native_locations(&source, &[wrong_turn, target.clone()], &cancel).unwrap();
    assert_eq!(found.len(), 1);

    let locator = found.get(&target).cloned().unwrap();
    let page = FileHistory::default()
        .around(session, target.1, Some(locator), 1)
        .await
        .unwrap()
        .unwrap();
    let [row] = page.entries.as_slice() else {
        panic!("one exact native opening")
    };
    assert_eq!(row.turn.as_ref(), Some(&target.0));
    assert!(row.text.is_empty());
    let [part] = row.parts.as_slice() else {
        panic!("only a question marker")
    };
    assert!(part.text.is_empty() && part.content.is_none());
    let VisiblePartKind::UserQuestion { request, questions } = &part.kind else {
        panic!("proven native question")
    };
    assert_eq!(request, &request_id("thread", turn, id));
    assert!(questions.is_empty());
}
