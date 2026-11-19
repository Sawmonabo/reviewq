//! Capture exact Serde bytes for the first-review development decoder.

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use reviewq_protocol::changes::{PatchAvailability, PatchFile, PatchFileIndex};
use reviewq_protocol::events::{
    InvalidateEvent, ResetEvent, ResetReasonDto, ResourceKeyDto, ResourceKindDto,
};
use reviewq_protocol::history::{
    HistoryAvailability, HistoryEntry, HistoryPage, HistoryPageEncodeError, HistoryPageResponse,
    HistoryPart, HistoryText, HistoryWindow, MAX_HISTORY_PAGE_BYTES,
};
use reviewq_protocol::queue::{
    CompactTicket, MergeRequestRow, MergeRequestStatus, NamedUpstreamId, QueueFacets, QueuePage,
    QueueRow, QueueStatus, QueueSync, QueueSyncMode, ReviewOverview, ReviewSelection, RunStage,
    RunSummary, UnmatchedTicket,
};
use reviewq_protocol::{
    CheckpointIdDto, DeliveryReceipt, DeliveryState, EventCursorDto, FileIdDto, GitShaDto,
    HistoryEntryIdDto, HistoryGenerationDto, HistoryTurnIdDto, HostProviderDto, MergeRequestIidDto,
    ObservedInputDto, Page, PatchETagDto, PatchRevisionDto, ProjectIdDto, ResourceIdDto,
    ReviewIdDto, RevisionDto, RunDetail, RunIdDto, SafeCountDto, SendMessageRequest, SessionKeyDto,
    SessionSummary, SubmissionIdDto, Success, TicketKeyDto, UpstreamIdDto, UtcTimestampDto,
};

const REVIEW: &str = "rq-alpha";
const RUN: &str = "run-active";
const LEAD: &str = "codex:active";
const TURN: &str = "active-turn-1";
const ENTRY_A: &str = "native-input-a";
const ENTRY_B: &str = "native-input-b";
const SAME_PROMPT: &str = "Reply with the same text.";
const SHA: &str = "000000000000000000000000000000000000000a";
const STAMP: &str = "2026-11-01T00:00:00.000Z";
const PATCH_ETAG: &str = "\"42b57a299915c31e805d8c61c892ef72b03652f047fdddae52d74e427d675f54\"";

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("missing fixture output path")?,
    );
    fs::create_dir(&output)?;
    emit_resources(&output)?;
    emit_history_and_receipt(&output)?;
    emit_request_and_events(&output)?;
    Ok(())
}

fn emit_resources(output: &Path) -> Result<(), Box<dyn Error>> {
    emit(
        output,
        "queue",
        &snapshot(QueuePage {
            page: Page::new(
                vec![
                    QueueRow::Mr(Box::new(review())),
                    QueueRow::UnmatchedTicket(UnmatchedTicket {
                        key: TicketKeyDto::new("RQ-44").unwrap(),
                        title: "Update the fixture".into(),
                        status: "In review".into(),
                        priority: Some("Medium".into()),
                        assignee: None,
                    }),
                ],
                None,
            )
            .unwrap(),
            total_rows: count(2),
            total_mrs: count(1),
            facets: QueueFacets {
                boards: Vec::new(),
                projects: vec![NamedUpstreamId {
                    id: UpstreamIdDto::new(11).unwrap(),
                    label: "forge/alpha".into(),
                }],
                sprints: Vec::new(),
                authors: Vec::new(),
                hide_reviewed: count(0),
                hide_in_progress: count(1),
            },
            sync: QueueSync {
                mode: QueueSyncMode::Connected,
                active: false,
                stale: false,
                last_attempt_at: Some(stamp()),
                last_success_at: Some(stamp()),
                error: None,
            },
            settings_revision: RevisionDto::new(7).unwrap(),
        }),
    )?;
    emit(
        output,
        "overview",
        &snapshot(ReviewOverview {
            review: review(),
            decision_revision: RevisionDto::new(0).unwrap(),
            decision: None,
            settings_revision: RevisionDto::new(7).unwrap(),
            checkpoint_id: Some(CheckpointIdDto::new("displayed-frontier").unwrap()),
            runs: Page::new(
                vec![RunSummary {
                    run_id: RunIdDto::new(RUN).unwrap(),
                    lead_session_key: SessionKeyDto::new(LEAD).unwrap(),
                    provider: HostProviderDto::Codex,
                    started_at: Some(stamp()),
                }],
                None,
            )
            .unwrap(),
        }),
    )?;
    emit(
        output,
        "selection",
        &snapshot(ReviewSelection::WithLead {
            review_id: ReviewIdDto::new(REVIEW).unwrap(),
            run_id: RunIdDto::new(RUN).unwrap(),
            lead_session_key: SessionKeyDto::new(LEAD).unwrap(),
        }),
    )?;
    emit(
        output,
        "selectionUnstarted",
        &snapshot(ReviewSelection::WithoutLead {
            review_id: ReviewIdDto::new(REVIEW).unwrap(),
            run_id: (),
            lead_session_key: (),
        }),
    )?;
    emit(
        output,
        "run",
        &snapshot(RunDetail {
            review_id: ReviewIdDto::new(REVIEW).unwrap(),
            run_id: RunIdDto::new(RUN).unwrap(),
            lead_session_key: SessionKeyDto::new(LEAD).unwrap(),
            sessions: Page::new(
                vec![SessionSummary {
                    session_key: SessionKeyDto::new(LEAD).unwrap(),
                    parent_session_key: None,
                    provider: HostProviderDto::Codex,
                }],
                None,
            )
            .unwrap(),
        }),
    )?;
    emit(
        output,
        "patchPending",
        &snapshot(patch_index("patch-a", SHA, None)),
    )?;
    emit(
        output,
        "patchA",
        &snapshot(patch_index("patch-a", SHA, Some(PATCH_ETAG))),
    )?;
    emit(
        output,
        "patchB",
        &snapshot(patch_index(
            "patch-b",
            "000000000000000000000000000000000000000b",
            Some(PATCH_ETAG),
        )),
    )?;
    Ok(())
}

fn emit_history_and_receipt(output: &Path) -> Result<(), Box<dyn Error>> {
    let page = history_page(vec![
        HistoryEntry {
            entry_id: HistoryEntryIdDto::new("active-item-0").unwrap(),
            turn_id: Some(HistoryTurnIdDto::new("active-turn-0").unwrap()),
            timestamp: None,
            parts: vec![HistoryPart::AssistantText {
                text: HistoryText::new("Ready.").unwrap(),
            }],
        },
        user_entry(ENTRY_B, SAME_PROMPT.into()),
        user_entry(ENTRY_A, SAME_PROMPT.into()),
    ]);
    let response = HistoryPageResponse::new(page, snapshot(()).cursor);
    fs::write(output.join("history.json"), response.encode()?)?;
    for (name, submission, entry) in [
        ("observedA", "send_identical_a", ENTRY_A),
        ("observedB", "send_identical_b", ENTRY_B),
    ] {
        emit(
            output,
            name,
            &snapshot(DeliveryReceipt {
                submission_id: SubmissionIdDto::new(submission).unwrap(),
                run_id: RunIdDto::new(RUN).unwrap(),
                session_key: SessionKeyDto::new(LEAD).unwrap(),
                state: DeliveryState::Started {
                    observed_input: ObservedInputDto {
                        turn_id: HistoryTurnIdDto::new(TURN).unwrap(),
                        entry_id: HistoryEntryIdDto::new(entry).unwrap(),
                    },
                },
                created_at: stamp(),
                updated_at: stamp(),
            }),
        )?;
    }

    let mut text = "\t🧪🧪".repeat(7_281);
    text.push_str("padding");
    assert_eq!(text.len(), 64 * 1024);
    let oversized = || {
        history_page(
            (0..5)
                .map(|index| user_entry(&format!("oversized-{index}"), text.clone()))
                .collect(),
        )
    };
    let response = HistoryPageResponse::new(oversized(), snapshot(()).cursor);
    assert!(matches!(
        response.encode(),
        Err(HistoryPageEncodeError::TooLarge)
    ));
    // A deliberately unbounded Serde capture proves the browser rejects these exact bytes.
    let bytes = serde_json::to_vec(&snapshot(oversized()))?;
    assert!(bytes.len() > MAX_HISTORY_PAGE_BYTES);
    fs::write(output.join("historyOversized.json"), bytes)?;
    Ok(())
}

fn history_page(items: Vec<HistoryEntry>) -> HistoryPage {
    HistoryPage {
        run_id: RunIdDto::new(RUN).unwrap(),
        session_key: SessionKeyDto::new(LEAD).unwrap(),
        generation: HistoryGenerationDto::new("source-generation").unwrap(),
        availability: HistoryAvailability::Available,
        reset: false,
        retry_after_ms: None,
        window: HistoryWindow::new(items, None, None).unwrap(),
    }
}

fn user_entry(entry: &str, text: String) -> HistoryEntry {
    HistoryEntry {
        entry_id: HistoryEntryIdDto::new(entry).unwrap(),
        turn_id: Some(HistoryTurnIdDto::new(TURN).unwrap()),
        timestamp: None,
        parts: vec![HistoryPart::UserPrompt {
            text: HistoryText::new(text).unwrap(),
        }],
    }
}

fn emit_request_and_events(output: &Path) -> Result<(), Box<dyn Error>> {
    let request: SendMessageRequest =
        serde_json::from_str(r#"{"submissionId":"send_42","text":"Reply with RQ41-WEB."}"#)?;
    assert!(
        serde_json::from_str::<SendMessageRequest>(
            r#"{"submissionId":"send_42","text":"Reply","actor":"checkout"}"#,
        )
        .is_err()
    );
    emit(output, "sendRequest", &request)?;
    emit(output, "eventReplayCursor", &snapshot(()).cursor)?;
    emit(
        output,
        "invalidate",
        &InvalidateEvent::new(vec![
            ResourceKeyDto {
                kind: ResourceKindDto::Run,
                id: ResourceIdDto::new(RUN),
            },
            ResourceKeyDto {
                kind: ResourceKindDto::Queue,
                id: None,
            },
        ])
        .unwrap(),
    )?;
    emit(
        output,
        "reset",
        &ResetEvent {
            reason: ResetReasonDto::CursorExpired,
        },
    )?;
    Ok(())
}

fn review() -> MergeRequestRow {
    MergeRequestRow {
        review_id: ReviewIdDto::new(REVIEW).unwrap(),
        project_id: ProjectIdDto::new(11).unwrap(),
        project_path: "forge/alpha".into(),
        iid: MergeRequestIidDto::new(41).unwrap(),
        title: "Worked review".into(),
        tickets: vec![CompactTicket {
            key: TicketKeyDto::new("RQ-41").unwrap(),
            title: "Update the fixture".into(),
        }],
        author: Some(NamedUpstreamId {
            id: UpstreamIdDto::new(7).unwrap(),
            label: "reviewer".into(),
        }),
        source_sha: GitShaDto::new(SHA).unwrap(),
        mr_state: MergeRequestStatus::Open,
        queue_state: QueueStatus::NeedsReview,
        updated_at: stamp(),
        draft: false,
        own: false,
        our_comments: count(0),
        unread_replies: count(0),
        personal_approval: Some(false),
        registered_lead_count: count(1),
        run_stage: Some(RunStage::Working),
        reason_kind: None,
        has_agent_questions: false,
    }
}

fn patch_index(revision: &str, head: &str, etag: Option<&str>) -> PatchFileIndex {
    PatchFileIndex {
        revision: PatchRevisionDto::new(revision).unwrap(),
        base_sha: GitShaDto::new("0000000000000000000000000000000000000001").unwrap(),
        start_sha: GitShaDto::new("0000000000000000000000000000000000000002").unwrap(),
        head_sha: GitShaDto::new(head).unwrap(),
        page: Page::new(
            vec![PatchFile {
                review_id: ReviewIdDto::new(REVIEW).unwrap(),
                revision: PatchRevisionDto::new(revision).unwrap(),
                file_id: FileIdDto::new("src-example").unwrap(),
                old_path: "src/example.ts".into(),
                new_path: "src/example.ts".into(),
                renamed: false,
                created: true,
                deleted: false,
                availability: PatchAvailability::Complete,
                byte_length: Some(count(42)),
                line_count: Some(count(2)),
                max_line_bytes: Some(count(27)),
                patch_etag: etag.and_then(PatchETagDto::new),
            }],
            None,
        )
        .unwrap(),
    }
}

fn snapshot<T>(data: T) -> Success<T> {
    Success {
        data,
        cursor: EventCursorDto::new(9_007_199_254_740_993).unwrap(),
    }
}

fn count(value: u64) -> SafeCountDto {
    SafeCountDto::new(value).unwrap()
}

fn stamp() -> UtcTimestampDto {
    UtcTimestampDto::new(STAMP).unwrap()
}

fn emit(output: &Path, name: &str, value: &impl serde::Serialize) -> Result<(), Box<dyn Error>> {
    let mut file = BufWriter::new(File::create_new(output.join(format!("{name}.json")))?);
    serde_json::to_writer(&mut file, value)?;
    writeln!(file)?;
    file.flush()?;
    Ok(())
}
