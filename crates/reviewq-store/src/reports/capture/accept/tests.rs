use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use reviewq_app::reports::{
    ArtifactChunk, ArtifactStats, HandoffState, ReportCaptureStore, ReportWork,
};
use reviewq_app::workflow::{NativeCall, WorkflowTool};
use reviewq_domain::identity::ReportHash;
use reviewq_domain::session::{AbsolutePath, LeadTarget};
use sha2::{Digest, Sha256};

use super::*;
use crate::SqliteStore;

const REPORT: &[u8] = b"# Verified review\n";
const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn report_admission_distinguishes_settled_history_from_competing_handoffs() {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let temp = tempfile::tempdir().unwrap();
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let path = AbsolutePath::from_canonical(temp.path().join("reports.sqlite3")).unwrap();
            let store = SqliteStore::open(path).await.unwrap();
            store.write(|tx| {
                tx.sql.execute_batch(
                    "INSERT INTO configured_projects VALUES('https://git.example',1);
                     INSERT INTO reviews(review_id,origin,project_id,iid,project_path,title,head_sha,state,updated_at_ms)
                       VALUES('review','https://git.example',1,1,'example/project','Review',printf('%040d',0),'open',0);
                     INSERT INTO runs VALUES('run','review','codex:lead',0),
                                            ('fresh','review','codex:new',0);
                     INSERT INTO sessions(session_key,run_id,provider,profile,native_id,cwd)
                       VALUES('codex:lead','run','codex',CAST('/codex' AS BLOB),'lead',CAST('/work' AS BLOB)),
                             ('codex:new','fresh','codex',CAST('/codex' AS BLOB),'new',CAST('/work' AS BLOB));",
                )?;
                Ok(())
            }).await.unwrap();
            let lead = store.report_lead("codex:lead".parse().unwrap()).await.unwrap();
            let source = file_source();
            let base = stage(&store, &lead, "initial", "older-file", final_report(None)).await;
            assert!(finish(&store, &base, true).await);
            let first = store.accepted_report(lead.clone()).await.unwrap().value.unwrap();
            let unchanged = Declaration::Unchanged {
                accepted: first.reference.clone(),
                source,
                assessments: Vec::new(),
            };
            let after_rotation =
                stage(&store, &lead, "retain", "current-file", unchanged.clone()).await;
            assert!(!finish(&store, &after_rotation, false).await);
            assert_eq!(recorded_state(&store, &after_rotation).await, HandoffState::Recorded);
            assert_eq!(store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference,
                first.reference);

            let amendment = stage(&store, &lead, "amend", "current-file",
                final_report(Some(first.reference.clone()))).await;
            assert!(finish(&store, &amendment, true).await);
            let current = store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference;
            assert_ne!(current.receipt_id, first.reference.receipt_id);
            assert_eq!(current.hash, first.reference.hash);

            let stale = stage(&store, &lead, "stale", "current-file", unchanged).await;
            assert!(!finish(&store, &stale, false).await);
            assert_eq!(recorded_state(&store, &stale).await, HandoffState::Failed);

            let full = stage(&store, &lead, "resumed-full", "rotated-file", final_report(None)).await;
            assert!(finish(&store, &full, true).await);
            let question = stage(&store, &lead, "question", "rotated-again",
                Declaration::NeedsInput { reason: "Choose a baseline revision.".into(), assessments: Vec::new() }
            ).await;
            assert!(!finish(&store, &question, false).await);
            assert_eq!(recorded_state(&store, &question).await, HandoffState::Recorded);
            let current = store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference;
            let exact = Declaration::Unchanged {
                accepted: current.clone(),
                source: file_source(),
                assessments: Vec::new(),
            };
            let changed_base =
                stage(&store, &lead, "changed-disposition", "rotated-again", exact.clone()).await;
            store.write(|tx| {
                tx.sql.execute("UPDATE report_heads SET disposition_version=1 WHERE run_id='run'", [])?;
                Ok(())
            }).await.unwrap();
            assert!(!finish(&store, &changed_base, false).await);
            assert_eq!(recorded_state(&store, &changed_base).await, HandoffState::Failed);

            let contested =
                stage(&store, &lead, "contested", "rotated-again", exact.clone()).await;
            let pending = stage(&store, &lead, "pending", "another-file", exact.clone()).await;
            assert!(!finish(&store, &contested, false).await);
            assert_eq!(recorded_state(&store, &contested).await, HandoffState::Failed);
            assert_eq!(recorded_state(&store, &pending).await, HandoffState::PendingCompletion);

            let before_settlement =
                stage(&store, &lead, "before-settlement", "rotated-again", exact).await;
            // Model a completion after admission without moving the accepted head or its disposition.
            store.write(|tx| {
                tx.sql.execute(
                    "UPDATE report_handoffs SET state='recorded',result_entry_id=call_id WHERE call_id='pending'",
                    [],
                )?;
                Ok(())
            }).await.unwrap();
            assert!(!finish(&store, &before_settlement, false).await);
            assert_eq!(recorded_state(&store, &before_settlement).await, HandoffState::Failed);
            assert_eq!(store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference,
                current);

            let other = store.report_lead("codex:new".parse().unwrap()).await.unwrap();
            let fresh = stage(&store, &other, "fresh", "brand-new-file", final_report(None)).await;
            assert!(finish(&store, &fresh, true).await);
            store.close(Duration::from_secs(1)).await.unwrap();
        });
}

async fn stage(
    store: &SqliteStore,
    lead: &LeadTarget,
    id: &str,
    generation: &str,
    declaration: Declaration,
) -> NativeCall {
    let source_position = match id {
        "initial" | "retain" | "pending" | "fresh" | "resumed-full" | "question" => 1,
        "amend" => 2,
        "stale" => 3,
        "changed-disposition" => 3,
        "contested" => 4,
        "before-settlement" => 5,
        _ => panic!("unknown fixture call"),
    };
    let call = NativeCall {
        lead: lead.clone(),
        turn_id: id.parse().unwrap(),
        root_turn_id: None,
        call_id: id.into(),
        source_generation: generation.into(),
        source_position,
        source_ordinal: 0,
        arguments_sha256: None,
        request_entry_id: None,
        tool: WorkflowTool::Report,
    };
    let authority = if declaration.requires_review_refresh() {
        Some(store.report_source(lead.clone()).await.unwrap().1)
    } else {
        None
    };
    let digest = digest(id.as_bytes());
    store
        .stage_report(
            call.clone(),
            declaration,
            digest.clone(),
            digest,
            authority,
            None,
        )
        .await
        .unwrap();
    call
}

async fn recorded_state(store: &SqliteStore, call: &NativeCall) -> HandoffState {
    store
        .report_handoff_status(call.clone(), digest(call.call_id.as_bytes()))
        .await
        .unwrap()
        .value
        .state
}

async fn finish(store: &SqliteStore, call: &NativeCall, new_version: bool) -> bool {
    let work = store
        .claim_report_work(4)
        .await
        .unwrap()
        .into_iter()
        .find(|work| &work.call == call)
        .expect("exact staged report can be claimed");
    let captured = if matches!(&work.declaration, Declaration::NeedsInput { .. }) {
        None
    } else {
        Some(capture(store, &work, new_version, REPORT).await)
    };
    store
        .accept_report(work, call.call_id.parse().unwrap(), captured, Vec::new())
        .await
        .unwrap()
}

async fn capture(
    store: &SqliteStore,
    work: &ReportWork,
    install: bool,
    body: &[u8],
) -> CapturedArtifact {
    let bytes = u64::try_from(body.len()).unwrap();
    if install {
        store.begin_artifact(work.clone()).await.unwrap();
        store
            .append_report_chunk(
                work.clone(),
                ArtifactChunk {
                    ordinal: 0,
                    byte_start: 0,
                    scalar_start: 0,
                    bytes: body.to_vec(),
                },
            )
            .await
            .unwrap();
    }
    CapturedArtifact {
        hash: digest(body),
        stats: ArtifactStats {
            byte_length: bytes,
            total_code_points: bytes,
            line_count: 1,
            max_line_bytes: bytes - 1,
            complete: true,
        },
        index: Some(Vec::new()),
        semantic_failure: false,
    }
}

fn final_report(amends: Option<ReportReference>) -> Declaration {
    Declaration::Final {
        source: file_source(),
        revision: RevisionEvidence::Exact(REVISION.parse().unwrap()),
        verdict: Verdict::RequestChanges,
        report: None,
        amends,
        edits: ReportEdits::default(),
        assessments: Vec::new(),
    }
}

fn file_source() -> ReportSource {
    ReportSource::File {
        path: AbsolutePath::from_canonical("/work/report.md".into()).unwrap(),
        hash: Some(digest(REPORT)),
    }
}

fn digest(bytes: &[u8]) -> ReportHash {
    ReportHash::from_sha256(Sha256::digest(bytes).into())
}
