use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use reviewq_app::reports::{
    ArtifactChunk, ArtifactStats, Collection, ContextRequest, Descriptor, FindingBasis,
    FindingBasisKind, FindingConclusion, FindingDetails, FindingEdit, FindingLocation,
    HandoffState, ReportCaptureStore, ReportEdits, ReportWork, SourceRange, StructuredFinding,
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
            let store = open(&temp).await;
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
            assert_eq!(recorded_state(&store, &changed_base).await, HandoffState::Recorded);

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

// BLITZ-901 and BLITZ-923: "Report update failed" stayed on a run after a newer report
// was saved. A failure stays listed, but the newest accepted report clears the ones before
// it, whichever was recorded first.
#[test]
fn accepted_report_resolves_every_earlier_delivery_failure() {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let store = open(&temp).await;
            let lead = store.report_lead("codex:lead".parse().unwrap()).await.unwrap();
            // The current head and a follow-up batch only satisfy the old rule's head guard and
            // give it a batch to mismatch; the rule now reads neither.
            store.write(|tx| {
                tx.sql.execute_batch(&format!(
                    "UPDATE reviews SET head_sha='{REVISION}';
                     INSERT INTO followup_monitors(review_id,run_id,generation,state,binding_verified,updated_at_ms)
                       VALUES('review','run',1,'paused',0,0);
                     INSERT INTO followup_batches(batch_id,review_id,run_id,lead_session_key,generation,state,
                       authority_verified,holds_slot,first_observed_at_ms,latest_observed_at_ms)
                       VALUES('batch','review','run','codex:lead',1,'completed',1,0,0,0);"
                ))?;
                Ok(())
            }).await.unwrap();
            let base = stage(&store, &lead, "base", "session", final_report(None)).await;
            assert!(finish(&store, &base, true).await);

            // BLITZ-901: a final failed in another follow-up scope than the next accepted final.
            // It fails after the newer one is staged, so its own failure time is not the order.
            let superseded = stage(&store, &lead, "superseded", "session", final_report(None)).await;
            let work = claimed(&store, &superseded).await;
            let replacement = stage(&store, &lead, "replacement", "session", final_report(None)).await;
            tokio::time::sleep(Duration::from_millis(2)).await;
            store.reject_report(work, None, "report_base_changed", None).await.unwrap();
            store.write(|tx| {
                tx.sql.execute("UPDATE report_handoffs SET batch_id='batch' WHERE call_id='superseded'", [])?;
                Ok(())
            }).await.unwrap();
            assert!(finish(&store, &replacement, true).await);

            // Two finals staged together: the later one is accepted first, then admission rejects
            // the earlier one, whose failure must not outlive the newer saved report.
            let overtaken = stage(&store, &lead, "overtaken", "session", final_report(None)).await;
            let overtaking = stage(&store, &lead, "overtaking", "session", final_report(None)).await;
            let mut works = store.claim_report_work(4).await.unwrap();
            assert_eq!(works.len(), 2);
            let earlier = works.remove(usize::from(works[0].call != overtaken));
            assert!(settle(&store, works.remove(0), true, REPORT, Vec::new()).await);
            assert!(!settle(&store, earlier, true, REPORT, Vec::new()).await);
            assert_eq!(recorded_state(&store, &overtaken).await, HandoffState::Failed);
            let current = store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference;

            // BLITZ-923: a turn ended without a valid handoff, then an unchanged result was accepted.
            assert!(store.reserve_report_correction(native_call(&lead, "corrected", "session")).await.unwrap());
            let unchanged = stage(&store, &lead, "unchanged", "session", Declaration::Unchanged {
                accepted: current,
                source: file_source(),
                assessments: Vec::new(),
            }).await;
            // A call in a rotated transcript, which native order cannot compare, asked for a
            // correction after the unchanged result was staged but before it was processed.
            tokio::time::sleep(Duration::from_millis(2)).await;
            assert!(store.reserve_report_correction(native_call(&lead, "later-correction", "rotated")).await.unwrap());
            assert!(!finish(&store, &unchanged, false).await);

            // Each failure names the report that cleared it: the overtaken final clears at its
            // own rejection, not at the next acceptance.
            let resolved = store.read(|db| {
                let mut statement = db.prepare(
                    "SELECT f.native_call_id,h.call_id FROM report_delivery_failures f
                     LEFT JOIN report_handoffs h ON h.receipt_id=f.resolved_by_receipt_id ORDER BY 1")?;
                let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                Ok(rows.collect::<Result<Vec<(String, Option<String>)>, _>>()?)
            }).await.unwrap();
            assert_eq!(resolved, [
                ("corrected".to_owned(), Some(unchanged.call_id)),
                ("later-correction".to_owned(), None),
                ("overtaken".to_owned(), Some(overtaking.call_id)),
                ("superseded".to_owned(), Some(replacement.call_id)),
            ]);
            store.close(Duration::from_secs(1)).await.unwrap();
        });
}

const LEGACY_PARENT: &str = "# Review

### F-01 Session token written to logs

Severity: P1
Blocking: yes

### F-02 Retry loop never ends

Severity: P1
Blocking: yes

### F-03 Missing regression test

Severity: P3
Blocking: no

### F-04 Stale comment

Severity: P3
Blocking: no
";

// BLITZ-605: a follow-up's Markdown moved the untouched P1 cards into a table. The test
// stages it with `stage_seeded_review`, as a full review with no batch bound.
const FOLLOWUP: &str = "# Follow-up review

### F-03 Missing regression test

Added in the retry suite.

### F-05 Cache key ignores tenant

Severity: P2
Blocking: no

## Earlier findings

| ID | Status |
| --- | --- |
| F-01 | Still present |
| F-02 | Still present |
";

#[test]
fn followup_carries_findings_its_text_and_ledger_omit() {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let store = open(&temp).await;
            let lead = store.report_lead("codex:lead".parse().unwrap()).await.unwrap();

            // A version accepted before the ledger kept the F-04 row it retired.
            let parent_call = stage(&store, &lead, "legacy-parent", "rollout",
                ledger_final(LEGACY_PARENT, Vec::new(), Vec::new(), None)).await;
            let parent_index = index(LEGACY_PARENT, &[
                ("### F-01", Some(("F-01", "P1", true))), ("### F-02", Some(("F-02", "P1", true))),
                ("### F-03", Some(("F-03", "P3", false))), ("### F-04", Some(("F-04", "P3", false))),
            ]);
            assert!(finish_with(&store, &parent_call, true, LEGACY_PARENT.as_bytes(), parent_index).await);
            let parent = store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference;
            let parent_id = parent.receipt_id.to_string();
            store.write(move |tx| {
                tx.sql.execute(
                    "INSERT INTO report_changes VALUES('run',?1,'F-04','fixed','Comment removed.',?2)",
                    params![parent_id, REVISION],
                )?;
                Ok(())
            }).await.unwrap();

            let followup_call = stage_seeded_review(&store, &lead, "ledger-followup", "rollout", ledger_final(
                FOLLOWUP,
                vec![StructuredFinding {
                    id: "F-05".into(),
                    title: "F-05 Cache key ignores tenant".into(),
                    severity: "P2".into(),
                    blocking: false,
                    details: FindingDetails {
                        location: Some(FindingLocation { path: "src/cache.rs".into(), line: Some(42) }),
                        consequence: "One tenant can read another tenant's cached page.".into(),
                        evidence: FindingBasis {
                            kind: FindingBasisKind::Observed,
                            text: "The key is built from the path alone.".into(),
                        },
                        checked_sha: None,
                        evidence_hash: None,
                    },
                }],
                vec![FindingEdit {
                    finding_id: "F-03".into(),
                    conclusion: FindingConclusion::Fixed,
                    reason: "The retry suite now covers it.".into(),
                    checked_sha: Some(REVISION.parse().unwrap()),
                    discussion_ids: Vec::new(),
                }],
                None,
            ), &["F-01", "F-02"]).await;
            let followup_index = index(FOLLOWUP, &[
                ("### F-03", Some(("F-03", "P3", false))), ("### F-05", Some(("F-05", "P2", false))),
                ("## Earlier", None),
            ]);
            assert!(finish_with(&store, &followup_call, true, FOLLOWUP.as_bytes(), followup_index.clone()).await);
            let followup = store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference;
            assert_carried(&store, &followup, &parent).await;
            let changes = context(&store, &followup, Collection::Changes).await;
            assert!(matches!(changes.as_slice(), [Descriptor::Change { id, conclusion: FindingConclusion::Fixed, .. }] if id == "F-03"));

            // An amendment keeps the details and origins its rows depend on.
            let amend_call = stage(&store, &lead, "followup-amend", "rollout",
                ledger_final(FOLLOWUP, Vec::new(), Vec::new(), Some(followup.clone()))).await;
            assert!(finish_with(&store, &amend_call, true, FOLLOWUP.as_bytes(), followup_index).await);
            let amendment = store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference;
            assert_ne!(amendment.receipt_id, followup.receipt_id);
            assert_carried(&store, &amendment, &parent).await;
            store.close(Duration::from_secs(1)).await.unwrap();
        });
}

// A report in another repository's format lists its findings in a table, without F- headings.
#[test]
fn unanchored_finding_keeps_details_and_origin_while_carried() {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let store = open(&temp).await;
            let lead = store.report_lead("codex:lead".parse().unwrap()).await.unwrap();
            let body = "# Review\n\n| ID | Finding |\n| --- | --- |\n| F-01 | Upload size is unbounded |\n";
            let finding = StructuredFinding {
                id: "F-01".into(),
                title: "Upload size is unbounded".into(),
                severity: "P1".into(),
                blocking: true,
                details: FindingDetails {
                    location: None,
                    consequence: "A large upload exhausts the worker's memory.".into(),
                    evidence: FindingBasis {
                        kind: FindingBasisKind::Gap,
                        text: "No limit is configured; the load test was not run.".into(),
                    },
                    checked_sha: None,
                    evidence_hash: None,
                },
            };
            let call = stage(&store, &lead, "ledger-only", "rollout",
                ledger_final(body, vec![finding.clone()], Vec::new(), None)).await;
            assert!(finish_with(&store, &call, true, body.as_bytes(), index(body, &[("# Review", None)])).await);
            let authored = store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference;
            let mut versions = Vec::new();
            for (id, body) in [("carry-once", "# Follow-up\n\nNothing new.\n"), ("carry-twice", "# Follow-up\n\nStill nothing new.\n")] {
                let call = stage_seeded_review(&store, &lead, id, "rollout",
                    ledger_final(body, Vec::new(), Vec::new(), None), &["F-01"]).await;
                assert!(finish_with(&store, &call, true, body.as_bytes(), Vec::new()).await);
                versions.push(store.accepted_report(lead.clone()).await.unwrap().value.unwrap().reference);
            }
            for version in &versions {
                let findings = context(&store, version, Collection::Findings).await;
                let [Descriptor::Finding { id, blocking, source: None, carried_from, details, .. }] = findings.as_slice() else {
                    panic!("one carried finding without a span");
                };
                assert_eq!((id.as_str(), *blocking), ("F-01", Some(true)));
                assert_eq!(carried_from.as_ref().map(|origin| &origin.report), Some(&authored),
                    "the origin is the version that authored the card, through a carried parent");
                let details = details.as_ref().expect("carried details are copied");
                assert_eq!(details.consequence, finding.details.consequence);
                assert_eq!(details.checked_sha.as_deref(), Some(REVISION));
                assert!(details.evidence_hash.is_some());
                let receipt = version.receipt_id.clone();
                let summary = store.read(move |db| {
                    Ok(crate::reports::version(db, &"run".parse().unwrap(), &receipt)?.summary)
                }).await.unwrap();
                assert_eq!((summary.finding_count, summary.unanchored_finding_count), (Some(1), 1));
            }
            store.close(Duration::from_secs(1)).await.unwrap();
        });
}

async fn assert_carried(store: &SqliteStore, version: &ReportReference, parent: &ReportReference) {
    let findings = context(store, version, Collection::Findings).await;
    let ids: Vec<&str> = findings
        .iter()
        .map(|item| match item {
            Descriptor::Finding { id, .. } => id.as_str(),
            _ => panic!("findings collection returned another item"),
        })
        .collect();
    assert_eq!(
        ids,
        ["F-01", "F-02", "F-05"],
        "F-03 is retired and F-04 stays retired"
    );
    for item in &findings[..2] {
        let Descriptor::Finding {
            blocking,
            source,
            carried_from,
            ..
        } = item
        else {
            unreachable!()
        };
        assert_eq!(*blocking, Some(true));
        assert_eq!(
            carried_from.as_ref().map(|origin| &origin.report),
            Some(parent)
        );
        let origin = source.as_ref().and_then(|range| range.origin.as_ref());
        assert_eq!(
            origin.map(|origin| &origin.report),
            Some(parent),
            "the parent's card supplies the span"
        );
    }
    let Descriptor::Finding {
        details,
        carried_from: None,
        source: Some(SourceRange { origin: None, .. }),
        ..
    } = &findings[2]
    else {
        panic!("F-05 is anchored to this report's heading");
    };
    let details = details.as_ref().expect("the ledger's details are kept");
    assert_eq!(details.checked_sha.as_deref(), Some(REVISION));
    let receipt = version.receipt_id.clone();
    let summary = store
        .read(move |db| Ok(crate::reports::version(db, &"run".parse().unwrap(), &receipt)?.summary))
        .await
        .unwrap();
    assert_eq!(summary.finding_count, Some(3));
    assert_eq!(summary.unanchored_finding_count, 2);
}

async fn context(
    store: &SqliteStore,
    version: &ReportReference,
    collection: Collection,
) -> Vec<Descriptor> {
    let receipt = version.receipt_id.clone();
    store
        .read(move |db| {
            Ok(crate::reports::context::read(
                db,
                &"run".parse().unwrap(),
                ContextRequest {
                    receipt: Some(receipt),
                    collection,
                    after: None,
                    limit: 50,
                },
            )?
            .items)
        })
        .await
        .unwrap()
}

fn ledger_final(
    body: &str,
    findings: Vec<StructuredFinding>,
    changes: Vec<FindingEdit>,
    amends: Option<ReportReference>,
) -> Declaration {
    Declaration::Final {
        source: file_source_for(body.as_bytes()),
        revision: RevisionEvidence::Exact(REVISION.parse().unwrap()),
        verdict: Verdict::RequestChanges,
        report: amends.is_none().then(|| {
            std::sync::Arc::new(StructuredReport {
                version: 1,
                findings,
            })
        }),
        amends,
        edits: ReportEdits { findings: changes },
        assessments: Vec::new(),
    }
}

/// Indexes the named headings of an ASCII report as the app's parser would.
/// A heading's marker and, for a finding card, its ID, severity and blocking line.
type Heading<'a> = (&'a str, Option<(&'a str, &'a str, bool)>);

fn index(body: &str, headings: &[Heading<'_>]) -> Vec<Descriptor> {
    let mut items = Vec::new();
    for (marker, finding) in headings {
        let start = body.find(marker).unwrap();
        let end = body[start + 1..]
            .find("\n#")
            .map_or(body.len(), |at| start + 2 + at);
        let title = body[start..]
            .lines()
            .next()
            .unwrap()
            .trim_start_matches('#')
            .trim();
        let source = SourceRange {
            offset: start as u64,
            length: (end - start) as u64,
            hash: digest(body[start..end].trim_end().as_bytes()),
            origin: None,
        };
        if let Some((id, severity, blocking)) = finding {
            items.push(Descriptor::Finding {
                id: (*id).into(),
                title: title.into(),
                severity: Some((*severity).into()),
                blocking: Some(*blocking),
                source: Some(source.clone()),
                struck: false,
                carried_from: None,
                details: None,
                disposition: None,
            });
        }
        items.push(Descriptor::Section {
            title: title.into(),
            source,
            cursor_offset: start as u64,
        });
    }
    items
}

async fn stage(
    store: &SqliteStore,
    lead: &LeadTarget,
    id: &str,
    generation: &str,
    declaration: Declaration,
) -> NativeCall {
    let call = native_call(lead, id, generation);
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
            None,
        )
        .await
        .unwrap();
    call
}

fn native_call(lead: &LeadTarget, id: &str, generation: &str) -> NativeCall {
    let source_position = match id {
        "initial" | "retain" | "pending" | "fresh" | "resumed-full" | "question" => 1,
        "amend" => 2,
        "stale" => 3,
        "changed-disposition" => 3,
        "contested" => 4,
        "before-settlement" => 5,
        "legacy-parent" => 1,
        "ledger-followup" => 2,
        "followup-amend" => 3,
        "ledger-only" => 1,
        "carry-once" => 2,
        "carry-twice" => 3,
        "base" => 1,
        "superseded" => 2,
        "replacement" => 3,
        "overtaken" => 4,
        "overtaking" => 5,
        "corrected" => 6,
        "unchanged" => 7,
        "later-correction" => 1,
        _ => panic!("unknown fixture call"),
    };
    NativeCall {
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
    }
}

/// Stages a full review seeded past the submit check; no batch is bound. The submit check
/// names each parent finding the ledger neither lists nor retires, so the fixture lists
/// the `carried` IDs while staging, then stores the declaration's own ledger in their place
/// so acceptance carries them.
async fn stage_seeded_review(
    store: &SqliteStore,
    lead: &LeadTarget,
    id: &str,
    generation: &str,
    declaration: Declaration,
    carried: &[&str],
) -> NativeCall {
    let Declaration::Final {
        report: Some(report),
        ..
    } = &declaration
    else {
        panic!("a follow-up final carries a ledger");
    };
    let ledger = serde_json::to_string(report.as_ref()).unwrap();
    let mut listed = declaration.clone();
    if let Declaration::Final {
        report: Some(report),
        ..
    } = &mut listed
    {
        let report = std::sync::Arc::make_mut(report);
        report
            .findings
            .extend(carried.iter().map(|id| listed_finding(id)));
    }
    let call = stage(store, lead, id, generation, listed).await;
    let call_id = call.call_id.clone();
    store
        .write(move |tx| {
            tx.sql.execute(
                "UPDATE report_handoffs SET edits=json_set(edits,'$.structured',json(?2)) WHERE call_id=?1",
                params![call_id, ledger],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    call
}

fn listed_finding(id: &str) -> StructuredFinding {
    StructuredFinding {
        id: id.into(),
        title: "Listed only while staging".into(),
        severity: "P3".into(),
        blocking: false,
        details: FindingDetails {
            location: None,
            consequence: "Replaced by the follow-up's own ledger.".into(),
            evidence: FindingBasis {
                kind: FindingBasisKind::Gap,
                text: "Not part of the fixture's ledger.".into(),
            },
            checked_sha: None,
            evidence_hash: None,
        },
    }
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
    finish_with(store, call, new_version, REPORT, Vec::new()).await
}

async fn finish_with(
    store: &SqliteStore,
    call: &NativeCall,
    new_version: bool,
    body: &[u8],
    index: Vec<Descriptor>,
) -> bool {
    let work = claimed(store, call).await;
    settle(store, work, new_version, body, index).await
}

/// Accepts claimed work and returns whether admission accepted it.
async fn settle(
    store: &SqliteStore,
    work: ReportWork,
    new_version: bool,
    body: &[u8],
    index: Vec<Descriptor>,
) -> bool {
    let captured = if matches!(&work.declaration, Declaration::NeedsInput { .. }) {
        None
    } else {
        Some(capture(store, &work, new_version, body, index).await)
    };
    let result = work.call.call_id.parse().unwrap();
    store.accept_report(work, result, captured).await.unwrap()
}

async fn claimed(store: &SqliteStore, call: &NativeCall) -> ReportWork {
    store
        .claim_report_work(4)
        .await
        .unwrap()
        .into_iter()
        .find(|work| &work.call == call)
        .expect("exact staged report can be claimed")
}

async fn capture(
    store: &SqliteStore,
    work: &ReportWork,
    install: bool,
    body: &[u8],
    index: Vec<Descriptor>,
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
        index: Some(index),
    }
}

fn final_report(amends: Option<ReportReference>) -> Declaration {
    Declaration::Final {
        source: file_source(),
        revision: RevisionEvidence::Exact(REVISION.parse().unwrap()),
        verdict: Verdict::RequestChanges,
        report: amends.is_none().then(|| {
            std::sync::Arc::new(StructuredReport {
                version: 1,
                findings: Vec::new(),
            })
        }),
        amends,
        edits: ReportEdits::default(),
        assessments: Vec::new(),
    }
}

fn file_source() -> ReportSource {
    file_source_for(REPORT)
}

fn file_source_for(body: &[u8]) -> ReportSource {
    ReportSource::File {
        path: AbsolutePath::from_canonical("/work/report.md".into()).unwrap(),
        hash: Some(digest(body)),
    }
}

async fn open(temp: &tempfile::TempDir) -> SqliteStore {
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
    store
}

fn digest(bytes: &[u8]) -> ReportHash {
    ReportHash::from_sha256(Sha256::digest(bytes).into())
}
