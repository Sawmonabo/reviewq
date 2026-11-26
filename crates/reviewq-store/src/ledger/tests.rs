use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use reviewq_app::access::Actor;
use reviewq_app::followups::{NextAction, ResultKind};
use reviewq_app::ledger::{
    CallBinding, CardSpan, ClosingCommit, ClosingRead, ClosingSnapshot, ClosingSummary, CommitKind,
    Composed, FailureRecord, FindingFields, FindingPatch, HostSession, LedgerEntries, LedgerRead,
    LedgerSelection, LedgerStore, NewVersion, NoteKey, NoteSection, NoteWrite, PublishCommit,
    PublishOutcome, RecordField, RecordReceipt, RecordWrite, SectionSpan, Span, VersionFacts,
    WriteRefusal,
};
use reviewq_app::reports::{
    ArtifactStats, Assessment, Collection, ContextRequest, Coverage, Descriptor, DispositionEdit,
    FindingBasis, FindingBasisKind, FindingConclusion, FindingDetails, FindingLocation,
    FindingSection, ReportDeliveryState, ReportEdits, ReportMutationStore, ReportStore,
    RevisionEvidence, Severity, Verdict,
};
use reviewq_domain::identity::{GitSha, ReportHash, Revision, RunId};
use reviewq_domain::ledger::{
    FailureKind, FindingId, PassKind, PassState, RecordRevision, Settlement, VersionKind, settle,
};
use reviewq_domain::session::AbsolutePath;
use sha2::{Digest, Sha256};

use crate::SqliteStore;

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn ledger_writes_are_idempotent() {
    run(async {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = AbsolutePath::from_canonical(temp.path().join("ledger.sqlite3")).unwrap();
        let store = open(path.clone()).await;
        let first = store
            .write_record(call("file"), RecordWrite::CreateFinding(Box::new(fields())))
            .await
            .unwrap()
            .unwrap();
        let replayed = store
            .write_record(call("file"), RecordWrite::CreateFinding(Box::new(fields())))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replayed, first);
        let equal = store
            .write_record(
                call("file-again"),
                RecordWrite::CreateFinding(Box::new(fields())),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(equal.receipt, first.receipt);
        let id = FindingId::FIRST;
        let retitle = FindingPatch {
            title: Some("Retry writes the invoice twice".into()),
            ..FindingPatch::default()
        };
        let changed = store
            .write_record(call("retitle"), change(RecordRevision::FIRST, retitle))
            .await
            .unwrap()
            .unwrap();
        let second = RecordRevision::FIRST.next().unwrap();
        assert!(
            matches!(changed.receipt, RecordReceipt::Finding { revision, .. } if revision == second)
        );
        let reseverity = FindingPatch {
            severity: Some(Severity::P2),
            ..FindingPatch::default()
        };
        let stale = store
            .write_record(call("stale"), change(RecordRevision::FIRST, reseverity))
            .await
            .unwrap();
        assert_eq!(
            stale,
            Err(WriteRefusal::StaleRevision {
                current: second,
                changed: vec![RecordField::Title],
            })
        );
        let note = NoteWrite {
            key: NoteKey::fixed(NoteSection::Summary).unwrap(),
            body: "Retries reuse the first key.".into(),
            expected_revision: None,
        };
        store
            .write_record(call("summary"), RecordWrite::WriteNote(note))
            .await
            .unwrap()
            .unwrap();
        store.close(Duration::from_secs(1)).await.unwrap();

        let store = open(path).await;
        let page = store.read_ledger(lead(), short()).await.unwrap();
        let LedgerEntries::Short { findings, notes } = page.entries else {
            panic!("a short read returns lines");
        };
        assert_eq!(
            findings
                .iter()
                .map(|line| (line.id, line.revision))
                .collect::<Vec<_>>(),
            [(id, second)]
        );
        assert_eq!(notes.len(), 1);
        assert_eq!(page.pass.map(|pass| pass.kind), Some(PassKind::FullReview));
        store.close(Duration::from_secs(1)).await.unwrap();
    });
}

#[test]
fn publish_is_conditional() {
    run(async {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = AbsolutePath::from_canonical(temp.path().join("publish.sqlite3")).unwrap();
        let store = open(path).await;
        let actor = Actor::local_operator();
        let run_id: RunId = "run".parse().unwrap();
        store
            .write_record(call("file"), RecordWrite::CreateFinding(Box::new(fields())))
            .await
            .unwrap()
            .unwrap();
        let pass = store
            .read_ledger(lead(), short())
            .await
            .unwrap()
            .pass
            .unwrap();
        store
            .record_failure(FailureRecord {
                run: run_id.clone(),
                pass: Some(pass.id),
                kind: FailureKind::UnfinishedReview,
                reason: "The lead ended without finishing; 1 finding is on file.".into(),
                sequence: Revision::try_from(1_i64).unwrap(),
                turn: None,
            })
            .await
            .unwrap();

        // A record filed after the snapshot makes the commit stale.
        let early = closing(&store, "finish").await;
        let note = NoteWrite {
            key: NoteKey::fixed(NoteSection::Summary).unwrap(),
            body: "Retries reuse the first key.".into(),
            expected_revision: None,
        };
        store
            .write_record(call("summary"), RecordWrite::WriteNote(note))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            store.commit_publish(full_review(early)).await.unwrap(),
            PublishOutcome::Stale
        );
        let view = store
            .read_report(actor.clone(), run_id.clone(), None, false)
            .await
            .unwrap()
            .value;
        assert!(view.report.is_none());
        assert_eq!(view.delivery.unwrap().state, ReportDeliveryState::Failed);

        // A current snapshot publishes, closes the pass and resolves the failure.
        let current = closing(&store, "finish").await;
        let PublishOutcome::Published(receipt) =
            store.commit_publish(full_review(current)).await.unwrap()
        else {
            panic!("a current snapshot publishes");
        };
        let view = store
            .read_report(actor.clone(), run_id.clone(), None, false)
            .await
            .unwrap()
            .value;
        assert_eq!(view.report.unwrap().reference.receipt_id, receipt);
        assert_eq!(view.delivery.unwrap().state, ReportDeliveryState::Saved);
        assert!(
            store
                .read_ledger(lead(), short())
                .await
                .unwrap()
                .pass
                .is_none()
        );
        let history = store
            .read_report_history(actor.clone(), run_id.clone(), None, 10)
            .await
            .unwrap();
        assert_eq!(history.value.items.len(), 1);
        assert_eq!(history.value.items[0].verdict, Verdict::RequestChanges);
        let context = store
            .read_report_context(
                actor.clone(),
                run_id.clone(),
                ContextRequest {
                    receipt: None,
                    collection: Collection::Findings,
                    after: None,
                    limit: 10,
                },
            )
            .await
            .unwrap()
            .value;
        let [
            Descriptor::Finding {
                id,
                details: Some(details),
                ..
            },
        ] = context.items.as_slice()
        else {
            panic!("the version's finding has its details");
        };
        assert_eq!(
            (id.as_str(), details.consequence.as_str()),
            ("F-01", fields().body.consequence.as_str())
        );

        // A disposition saved after the snapshot makes the commit stale.
        let raced = closing(&store, "finish-again").await;
        store
            .set_disposition(
                actor.clone(),
                DispositionEdit {
                    run: run_id.clone(),
                    receipt: receipt.clone(),
                    expected_revision: Revision::try_from(0_i64).unwrap(),
                    finding_id: "F-01".into(),
                    submission: uuid::Uuid::new_v4().to_string().parse().unwrap(),
                    blocking: Some(false),
                    publication: None,
                    reason: "Accepted risk for this release.".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            store.commit_publish(full_review(raced)).await.unwrap(),
            PublishOutcome::Stale
        );
        let history = store
            .read_report_history(actor.clone(), run_id.clone(), None, 10)
            .await
            .unwrap();
        assert_eq!(history.value.items.len(), 1);

        // A selected-thread close that changed nothing completes the batch as unchanged.
        // The idle lead has not started the delivered batch, so the run does not settle yet.
        select_thread(&store, receipt.as_str()).await;
        assert_eq!(lead_idle(&store).await, [false]);
        activity(&store, "working").await;
        activity(&store, "idle").await;
        assert_eq!(lead_idle(&store).await, [true]);
        let selected = closing(&store, "finish-thread").await;
        assert_eq!(
            selected.snapshot.pass.as_ref().map(|pass| pass.kind),
            Some(PassKind::SelectedThreads)
        );
        let commit = PublishCommit {
            guard: selected.snapshot.guard,
            kind: CommitKind::Closing(Box::new(ClosingCommit {
                call: selected.caller,
                summary: summary(ResultKind::Unchanged),
                assessments: vec![Assessment {
                    discussion_id: "D1".into(),
                    conclusion: FindingConclusion::StillPresent,
                    reason: "The retry still builds a fresh key.".into(),
                    finding_ids: vec!["F-01".into()],
                    next_action: NextAction::Review,
                }],
                version: None,
            })),
        };
        assert_eq!(
            store.commit_publish(commit).await.unwrap(),
            PublishOutcome::Unchanged
        );
        let history = store
            .read_report_history(actor, run_id, None, 10)
            .await
            .unwrap();
        assert_eq!(history.value.items.len(), 1);
        let completed: (String, String) = store
            .read(|db| {
                Ok(db.query_row(
                    "SELECT b.state, r.kind FROM followup_batches b
                     JOIN followup_results r USING(review_id, batch_id) WHERE b.batch_id='batch'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(completed, ("completed".into(), "unchanged".into()));
        store.close(Duration::from_secs(1)).await.unwrap();
    });
}

#[test]
fn pass_hold_and_start_follow_lead_activity() {
    run(async {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = AbsolutePath::from_canonical(temp.path().join("passes.sqlite3")).unwrap();
        let store = open(path).await;

        // The lead was seen working before its launch's pass opened, so the pass has started
        // and settles once the lead is idle.
        activity(&store, "working").await;
        store
            .write(|tx| {
                tx.sql.execute(
                    "INSERT INTO launch_attempts(id,actor,submission_id,review_id,request_digest,
                       settings_revision,source_sha,provider,checkout,common_dir,invocation,profile,
                       native_id,run_id,session_key,state,created_at_ms,updated_at_ms)
                     VALUES('launch',zeroblob(32),'launch','review',zeroblob(32),0,?1,'claude',
                       CAST('/work' AS BLOB),CAST('/work' AS BLOB),'{}',CAST('/claude' AS BLOB),'lead',
                       'run','claude:lead','confirmed',0,0)",
                    [SHA],
                )?;
                crate::ledger::passes::open_for_launch(&tx.sql, "launch", 0)
            })
            .await
            .unwrap();
        // A lead whose activity is unknown, as for a Codex thread running in another
        // process, does not settle; the same lead stored idle does.
        activity(&store, None).await;
        assert_eq!(settlements(&store).await, [Settlement::Nothing]);
        activity(&store, "idle").await;
        assert_eq!(settlements(&store).await, [Settlement::Unfinished]);

        // A hold made before the host reported its turn as working survives the host's first
        // sight of that turn, and reopens when the next turn starts without any call.
        store
            .hold_pass(call("ask"), "Which branch is the base?".into())
            .await
            .unwrap()
            .unwrap();
        let state = async || {
            let page = store.read_ledger(lead(), short()).await.unwrap();
            page.pass.map(|pass| (pass.state, pass.question))
        };
        // The question is read back while the pass waits and kept once it reopens, so what
        // the pass owes can name it.
        let question = || Some("Which branch is the base?".to_owned());
        let waiting = Some((PassState::Waiting, question()));
        activity(&store, "working").await;
        assert_eq!(state().await, waiting);
        activity(&store, "idle").await;
        assert_eq!(state().await, waiting);
        // Losing and regaining sight of the idle lead is no new turn.
        activity(&store, None).await;
        activity(&store, "idle").await;
        assert_eq!(state().await, waiting);
        activity(&store, "working").await;
        assert_eq!(state().await, Some((PassState::Open, question())));

        // The lead's own record write proves its turn is running, so an idle state stored
        // before it is stale: the run settles only once the host reports the lead idle again.
        activity(&store, "idle").await;
        store
            .write_record(call("file"), RecordWrite::CreateFinding(Box::new(fields())))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settlements(&store).await, [Settlement::Nothing]);
        activity(&store, "idle").await;
        assert_eq!(settlements(&store).await, [Settlement::Unfinished]);
        store.close(Duration::from_secs(1)).await.unwrap();
    });
}

fn run(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(test);
}

/// Opens the store and, on a fresh database, registers one Claude lead on one review.
async fn open(path: AbsolutePath) -> SqliteStore {
    let store = SqliteStore::open(path).await.unwrap();
    store
        .write(|tx| {
            tx.sql.execute_batch(
                "INSERT OR IGNORE INTO configured_projects VALUES('https://git.example',1);
                 INSERT OR IGNORE INTO reviews(review_id,origin,project_id,iid,project_path,title,head_sha,state,updated_at_ms)
                   VALUES('review','https://git.example',1,7,'billing/api','Bound invoice retries',printf('%040d',0),'open',0);
                 INSERT OR IGNORE INTO runs VALUES('run','review','claude:lead',0);
                 INSERT OR IGNORE INTO sessions(session_key,run_id,provider,profile,native_id,cwd,host_activity)
                   VALUES('claude:lead','run','claude',CAST('/claude' AS BLOB),'lead',CAST('/work' AS BLOB),'idle');",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    store
}

fn lead() -> HostSession {
    HostSession::Claude {
        session: "claude:lead".parse().unwrap(),
        agent: None,
    }
}

fn call(id: &str) -> CallBinding {
    CallBinding {
        session: lead(),
        call_id: id.into(),
        turn: Some("turn-1".parse().unwrap()),
    }
}

fn short() -> LedgerRead {
    LedgerRead {
        selection: LedgerSelection::Short,
        after: None,
    }
}

fn fields() -> FindingFields {
    FindingFields {
        title: "Retry writes the same invoice twice".into(),
        severity: Severity::P1,
        blocking: true,
        section: FindingSection::Functional,
        category: None,
        body: FindingDetails {
            locations: vec![FindingLocation {
                path: "src/invoices.rs".into(),
                line: Some(74),
            }],
            issue: "The retry builds a fresh idempotency key after the first insert commits."
                .into(),
            consequence: "A lost acknowledgement creates a duplicate invoice.".into(),
            evidence: FindingBasis {
                kind: FindingBasisKind::Observed,
                text: "Replaying the request after a dropped reply inserted two rows.".into(),
            },
            proposed_comment: None,
            details: None,
        },
    }
}

fn change(expected: RecordRevision, patch: FindingPatch) -> RecordWrite {
    RecordWrite::ChangeFinding {
        id: FindingId::FIRST,
        expected_revision: expected,
        patch: Box::new(patch),
    }
}

async fn closing(store: &SqliteStore, id: &str) -> ClosingSnapshot {
    match store.closing_snapshot(call(id)).await.unwrap() {
        ClosingRead::Current(snapshot) => *snapshot,
        _ => panic!("the lead's new closing call reads a snapshot"),
    }
}

fn summary(result: ResultKind) -> ClosingSummary {
    ClosingSummary {
        result,
        new: Vec::new(),
        updated: Vec::new(),
        retired: Vec::new(),
        carried: 0,
        by_subagents: Vec::new(),
        empty_sections: Vec::new(),
    }
}

/// A full review whose text holds one card per active finding and one section.
fn full_review(closing: ClosingSnapshot) -> PublishCommit {
    let mut text =
        String::from("[billing/api · !7](https://git.example/billing/api/-/merge_requests/7)\n\n");
    let mut cards = Vec::new();
    for finding in closing
        .snapshot
        .findings
        .iter()
        .filter(|finding| finding.record.is_active())
    {
        let start = text.len();
        text.push_str(&format!(
            "### {} {}\n\n",
            finding.record.id, finding.record.fields.title
        ));
        cards.push(CardSpan {
            id: finding.record.id,
            span: span(&text, start),
        });
    }
    let start = text.len();
    text.push_str("## Verification and limits\n\nNot recorded.\n");
    let sections = vec![SectionSpan {
        title: "Verification and limits".into(),
        span: span(&text, start),
    }];
    let stats = ArtifactStats {
        byte_length: text.len() as u64,
        total_code_points: text.chars().count() as u64,
        line_count: text.lines().count() as u64,
        max_line_bytes: text.lines().map(str::len).max().unwrap_or(0) as u64,
    };
    let sha: GitSha = SHA.parse().unwrap();
    PublishCommit {
        guard: closing.snapshot.guard,
        kind: CommitKind::Closing(Box::new(ClosingCommit {
            call: closing.caller,
            summary: summary(ResultKind::Final),
            assessments: Vec::new(),
            version: Some(NewVersion {
                facts: VersionFacts {
                    kind: VersionKind::FullReview,
                    verdict: Verdict::RequestChanges,
                    reviewed: RevisionEvidence::Exact(sha.clone()),
                    coverage: Coverage::FullMr,
                    coverage_source_sha: None,
                    coverage_target_sha: None,
                    full_mr_reviewed_sha: Some(sha),
                    checked_at_ms: closing.snapshot.cover.read_at_ms,
                    new_attention: true,
                },
                composed: Composed {
                    sha256: ReportHash::from_sha256(Sha256::digest(text.as_bytes()).into()),
                    stats,
                    cards,
                    sections,
                    text,
                },
                changes: ReportEdits::default(),
            }),
        })),
    }
}

/// The span from byte `start` to the end of `text`, which is ASCII here.
fn span(text: &str, start: usize) -> Span {
    Span {
        bytes: start..text.len(),
        scalar_offset: start as u64,
        scalar_length: (text.len() - start) as u64,
    }
}

/// The settle read's `lead_idle` for each candidate run.
async fn lead_idle(store: &SqliteStore) -> Vec<bool> {
    let candidates = store.runs_to_settle().await.unwrap();
    candidates.iter().map(|run| run.facts.lead_idle).collect()
}

/// What settling does for each candidate run.
async fn settlements(store: &SqliteStore) -> Vec<Settlement> {
    let candidates = store.runs_to_settle().await.unwrap();
    candidates.iter().map(|run| settle(run.facts)).collect()
}

/// Stores the lead's observed activity, or `None` for unknown, as the host adapters do.
async fn activity(store: &SqliteStore, phase: impl Into<Option<&'static str>>) {
    let phase = phase.into();
    store
        .write(move |tx| crate::hosts::record_activity(&tx.sql, "claude:lead", phase, false))
        .await
        .unwrap();
}

/// Starts a selected-thread batch for discussion D1, which tracks F-01, against `receipt`.
async fn select_thread(store: &SqliteStore, receipt: &str) {
    let manifest = serde_json::json!({
        "batchId": "batch", "submissionId": "submission", "generation": 1, "sourceSha": SHA,
        "targetSha": null, "reportReceiptId": receipt, "reportHash": null, "dispositionVersion": 1,
        "model": "model", "effort": "high",
        "observations": [{"id": "observation", "discussionId": "D1", "discussionVersion": "v1",
            "originals": [], "findingIds": ["F-01"], "priorBatches": []}],
    })
    .to_string();
    let receipt = receipt.to_owned();
    store
        .write(move |tx| {
            tx.sql.execute(
                "INSERT INTO followup_monitors(review_id,run_id,generation,state,reviewer_id,binding_verified,updated_at_ms)
                 VALUES('review','run',1,'enabled','7',1,0)",
                [],
            )?;
            tx.sql.execute(
                "INSERT INTO followup_batches(batch_id,review_id,run_id,lead_session_key,generation,state,source_sha,
                   base_receipt_id,disposition_version,private_manifest,authority_verified,holds_slot,submission_id,
                   native_turn_id,first_observed_at_ms,latest_observed_at_ms)
                 VALUES('batch','review','run','claude:lead',1,'executing',?1,?2,1,?3,1,1,'submission','turn-1',0,0)",
                rusqlite::params![SHA, receipt, manifest],
            )?;
            crate::ledger::passes::open_for_batch(&tx.sql, &"review".parse().unwrap(), "batch", 0)
        })
        .await
        .unwrap();
}
