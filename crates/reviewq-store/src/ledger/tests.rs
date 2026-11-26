use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use reviewq_app::access::{Actor, AppError};
use reviewq_app::followups::{NextAction, ResultKind};
use reviewq_app::hosts::{
    ClaudeClassicEvent, ClaudeHeartbeat, ClaudeHost, ClaudeStore, ClaudeWorkflowStore,
    HostActivity, HostPhase,
};
use reviewq_app::ledger::publish::{finish_review, settle_due};
use reviewq_app::ledger::{
    CallBinding, CardSpan, ClosingCommit, ClosingRead, ClosingSnapshot, ClosingSummary, CommitKind,
    Composed, FailureRecord, FindingAssessment, FindingFields, FindingOutcome, FindingPatch,
    FinishReceipt, FinishReview, HostSession, LedgerEntries, LedgerRead, LedgerSelection,
    LedgerStore, NewVersion, NoteKey, NoteSection, NoteWrite, PublishCommit, PublishOutcome,
    RecordField, RecordReceipt, RecordWrite, ReportLinks, SectionSpan, Span, VersionFacts,
    WriteRefusal,
};
use reviewq_app::notifications::{InboxStore, InboxView, Kind};
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
use crate::queue::lead_for_run;

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
        file(&store, "file", &fields().title).await;
        let ledger = store.read_ledger(lead(), short());
        let pass = ledger.await.unwrap().pass.unwrap();
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
        assert_eq!(inbox(&store).await, [Kind::ReportFailed]);

        // A record filed after the snapshot makes the commit stale.
        let early = closing(&store, "finish").await;
        let note = NoteWrite {
            key: NoteKey::fixed(NoteSection::Summary).unwrap(),
            body: "Retries reuse the first key.".into(),
            expected_revision: None,
        };
        let write = store.write_record(call("summary"), RecordWrite::WriteNote(note));
        write.await.unwrap().unwrap();
        let outcome = store.commit_publish(full_review(early)).await;
        assert_eq!(outcome.unwrap(), PublishOutcome::Stale);
        let view = store.read_report(actor.clone(), run_id.clone(), None, false);
        let view = view.await.unwrap().value;
        assert!(view.report.is_none());
        assert_eq!(view.delivery.unwrap().state, ReportDeliveryState::Failed);

        // A current snapshot publishes, closes the pass, resolves the failure and takes its
        // notification out of the inbox.
        let current = closing(&store, "finish").await;
        let PublishOutcome::Published(receipt) =
            store.commit_publish(full_review(current)).await.unwrap()
        else {
            panic!("a current snapshot publishes");
        };
        assert_eq!(inbox(&store).await, [Kind::ReportReady]);
        let view = store.read_report(actor.clone(), run_id.clone(), None, false);
        let view = view.await.unwrap().value;
        assert_eq!(view.report.unwrap().reference.receipt_id, receipt);
        assert_eq!(view.delivery.unwrap().state, ReportDeliveryState::Saved);
        let ledger = store.read_ledger(lead(), short());
        assert!(ledger.await.unwrap().pass.is_none());
        let history = store.read_report_history(actor.clone(), run_id.clone(), None, 10);
        let history = history.await.unwrap();
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
        let outcome = store.commit_publish(full_review(raced)).await;
        assert_eq!(outcome.unwrap(), PublishOutcome::Stale);
        let history = store.read_report_history(actor.clone(), run_id.clone(), None, 10);
        assert_eq!(history.await.unwrap().value.items.len(), 1);

        // A selected-thread close that changed nothing completes the batch as unchanged.
        // The idle lead has not started the delivered batch, so the run does not settle yet.
        select_thread(&store, receipt.as_str()).await;
        assert_eq!(lead_idle(&store).await, [false]);
        // After a restart a companion that has not reported yet is unknown: neither idle for
        // dispatch nor settled, whatever it last reported, until the watch lets it be gone.
        let companion = "INSERT INTO claude_companions(session_key,generation,sequence,seen_at_ms,
            turn_id,latest_prompt_manual) VALUES('claude:lead',printf('%032d',0),1,1000,'turn-1',0)";
        let inserted = store.write(|tx| Ok(tx.sql.execute(companion, [])?));
        inserted.await.unwrap();
        let lead = store.read(|db| Ok(lead_for_run(db, &"run".parse().unwrap())?.unwrap()));
        let lead = lead.await.unwrap();
        let live = async || {
            let context = store.claude_workflow_context(lead.clone(), None);
            let context = context.await.unwrap();
            (context.active_turn.is_some(), context.idle)
        };
        activity(&store, "working").await;
        assert_eq!(lead_idle(&store).await, [false]);
        assert_eq!(live().await, (false, false));
        // A permission prompt from Claude's own hook shows the session present, and is kept.
        let prompt =
            store.record_claude_classic(lead.clone(), ClaudeClassicEvent::PermissionPending);
        prompt.await.unwrap();
        activity(&store, "idle").await;
        assert_eq!(lead_idle(&store).await, [false]);
        assert_eq!(live().await, (false, false));
        // The prompt binds to the turn and step of the companion's next report, with one alert.
        let (phase, has_questions) = (HostPhase::Working, false);
        let report = ClaudeHeartbeat {
            generation: "0".repeat(32),
            sequence: 2,
            activity: Some(HostActivity {
                phase,
                has_questions,
            }),
            turn: Some("turn-2".parse().unwrap()),
            step: Some(3),
            question_id: None,
            latest_prompt: None,
            latest_prompt_turn: None,
            latest_prompt_manual: false,
            model: None,
            effort: None,
            host: ClaudeHost::of(std::process::id()),
        };
        let reported = store.record_claude_heartbeat(lead.clone(), report);
        reported.await.unwrap();
        let shown = store.read(|db| {
            let sql = "SELECT host_activity, (SELECT group_concat(event_ref) FROM notifications
                WHERE kind='agent_permission') FROM sessions WHERE session_key='claude:lead'";
            Ok(db.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?)))?)
        });
        let shown: (String, Option<String>) = shown.await.unwrap();
        assert_eq!(shown, ("blocked".into(), Some("turn-2".into())));
        // Its host is this test's own process, trusted as two reports 5 seconds apart make it.
        let silent = "UPDATE claude_companions SET seen_at_ms=1000, host_trusted=1";
        let silenced = store.write(|tx| Ok(tx.sql.execute(silent, [])?));
        silenced.await.unwrap();
        // A ReviewQ stopped for a minute has not watched it, and its live host proves nothing:
        // the companion is still unknown. A host process that no longer exists ends it at once.
        store.companions.watched_long_enough();
        store.companions.stopped_for(Duration::from_secs(60));
        store.end_companions().await.unwrap();
        assert_eq!(lead_idle(&store).await, [false]);
        let dead = "UPDATE claude_companions SET host_pid=4294967295";
        store
            .write(|tx| Ok(tx.sql.execute(dead, [])?))
            .await
            .unwrap();
        store.end_companions().await.unwrap();
        assert_eq!(lead_idle(&store).await, [true]);
        let selected = closing(&store, "finish-thread").await;
        let kind = selected.snapshot.pass.as_ref().map(|pass| pass.kind);
        assert_eq!(kind, Some(PassKind::SelectedThreads));
        // The cover carries the review's Jira link, its ticket and the note its monitored
        // discussion resolves to.
        let cover = &selected.snapshot.cover;
        let jira = Some("https://example.atlassian.net");
        let links = ReportLinks::new("https://git.example", "billing/api", cover.iid, jira);
        assert_eq!(cover.links, links.unwrap());
        assert_eq!(
            (cover.tickets.as_slice(), cover.thread_notes.get("D1")),
            (["RQ-41".parse().unwrap()].as_slice(), Some(&"9001".into()))
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

        // A full review covers what the lead did since it last closed, even when settling
        // published an update in between.
        assess(&store, "assess-1", 1).await;
        file(&store, "file-2", "Refund skips the ledger lock").await;
        activity(&store, "idle").await;
        settle_due(&store).await.unwrap();
        assert_eq!(versions(&store).await, ["full_review", "update"]);
        finish_full_review(&store, "close-full").await.unwrap();
        // A finding on the current version that the lead neither changed nor assessed since
        // that close still refuses one. An assessment equal to the stored one counts.
        file(&store, "file-3", "Export drops the currency").await;
        assert_eq!(untouched(&store, "close-skipped").await, ["F-01", "F-02"]);
        assess(&store, "assess-1-again", 2).await;
        assert_eq!(untouched(&store, "close-one-left").await, ["F-02"]);
        reseverity(&store, "reseverity-2", 1, Severity::P2).await;
        let closed = finish_full_review(&store, "close-all").await.unwrap();
        assert_eq!(
            versions(&store).await,
            ["full_review", "update", "full_review", "full_review"]
        );
        // A closing call answered with the current version still moves where the next full
        // review starts, also for a full review whose snapshot was read before it.
        assess(&store, "assess-1-equal", 2).await;
        let racing = closing(&store, "close-racing").await;
        let input = FinishReview {
            verdict: None,
            revision: None,
            full_review: false,
            assessments: Vec::new(),
        };
        let answered = finish_review(&store, call("close-answered"), input).await;
        assert_eq!(answered.unwrap().receipt, closed.receipt);
        let raced = store.commit_publish(full_review(racing)).await;
        assert_eq!(raced.unwrap(), PublishOutcome::Stale);
        reseverity(&store, "reseverity-2-again", 2, Severity::P1).await;
        let refused = untouched(&store, "close-after-answer").await;
        assert_eq!(refused, ["F-01", "F-03"]);
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
    let mut text = String::from(
        "[api](https://git.example/billing/api) · [MR !7](https://git.example/billing/api/-/merge_requests/7)\n\n",
    );
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

/// The kinds of the notifications the inbox lists, read or unread.
async fn inbox(store: &SqliteStore) -> Vec<Kind> {
    let page = store
        .read_inbox(Actor::local_operator(), InboxView::All, None, 10)
        .await
        .unwrap();
    page.value.items.iter().map(|item| item.kind).collect()
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
            tx.sql.execute_batch(
                "INSERT INTO followup_monitors(review_id,run_id,generation,state,reviewer_id,binding_verified,updated_at_ms)
                 VALUES('review','run',1,'enabled','7',1,0);
                 INSERT INTO followup_threads(review_id,generation,original_note_id,discussion_id,reviewer_id,selected)
                   VALUES('review',1,'9001','D1','7',0);
                 INSERT INTO followup_thread_findings VALUES('review',1,'9001','F-01');
                 INSERT INTO settings(provider,origin,email,token_type,cloud_id,identity_id,identity_label)
                   VALUES('jira','https://example.atlassian.net','reviewer@example.com','scoped',
                     printf('%036d',0),'reviewer','Reviewer');
                 INSERT INTO scoped_issues(board_id,ticket_key,title,project_key,status_id,status)
                   VALUES(1,'RQ-41','Bound invoice retries','RQ',1,'Open');
                 INSERT INTO scoped_review_issues VALUES('review',1,'RQ-41');",
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

/// The lead assesses F-01 as still present, expecting `revision`.
async fn assess(store: &SqliteStore, id: &str, revision: i64) {
    let assessment = FindingAssessment {
        id: FindingId::FIRST,
        expected_revision: RecordRevision::try_from(revision).unwrap(),
        outcome: FindingOutcome {
            conclusion: FindingConclusion::StillPresent,
            reason: "The retry still builds a fresh key.".into(),
            discussion_ids: Vec::new(),
            checked_sha: None,
        },
    };
    store
        .write_record(call(id), RecordWrite::AssessFinding(assessment))
        .await
        .unwrap()
        .unwrap();
}

/// The lead files a finding like `fields()` titled `title`.
async fn file(store: &SqliteStore, id: &str, title: &str) {
    let mut fields = fields();
    fields.title = title.into();
    let create = RecordWrite::CreateFinding(Box::new(fields));
    store.write_record(call(id), create).await.unwrap().unwrap();
}

/// The lead's `finish_review` with `fullReview`, through the app's closing checks.
async fn finish_full_review(store: &SqliteStore, id: &str) -> Result<FinishReceipt, AppError> {
    let input = FinishReview {
        verdict: Some(Verdict::RequestChanges),
        revision: Some(RevisionEvidence::Exact(SHA.parse().unwrap())),
        full_review: true,
        assessments: Vec::new(),
    };
    finish_review(store, call(id), input).await
}

/// The lead sets F-02's severity, expecting `revision`.
async fn reseverity(store: &SqliteStore, id: &str, revision: i64, severity: Severity) {
    let patch = FindingPatch {
        severity: Some(severity),
        ..FindingPatch::default()
    };
    let change = RecordWrite::ChangeFinding {
        id: FindingId::FIRST.next().unwrap(),
        expected_revision: RecordRevision::try_from(revision).unwrap(),
        patch: Box::new(patch),
    };
    store.write_record(call(id), change).await.unwrap().unwrap();
}

/// The finding IDs a full review closed with `id` names as left untouched, in order.
async fn untouched(store: &SqliteStore, id: &str) -> Vec<String> {
    let Err(AppError::InvalidFields(errors)) = finish_full_review(store, id).await else {
        panic!("a skipped finding is a field error");
    };
    errors
        .iter()
        .map(|error| error.message[..4].to_owned())
        .collect()
}

/// The kinds of the run's versions, oldest first.
async fn versions(store: &SqliteStore) -> Vec<String> {
    store
        .read(|db| {
            let mut statement = db.prepare(
                "WITH RECURSIVE chain(run_id, receipt_id, depth) AS (
                   SELECT run_id, receipt_id, 0 FROM report_heads
                   UNION ALL SELECT v.run_id, v.parent_receipt_id, c.depth + 1 FROM chain c
                   JOIN report_versions v USING(run_id, receipt_id)
                   WHERE v.parent_receipt_id IS NOT NULL)
                 SELECT v.kind FROM chain c JOIN report_versions v USING(run_id, receipt_id)
                 ORDER BY c.depth DESC",
            )?;
            let kinds = statement.query_map([], |row| row.get(0))?;
            Ok(kinds.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap()
}
