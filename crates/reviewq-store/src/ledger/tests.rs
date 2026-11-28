use reviewq_app::ledger::{
    CallBinding, FindingFields, HostSession, LedgerRead, LedgerSelection, LedgerStore, NoteKey,
    NoteSection, NoteWrite, RecordWrite,
};
use reviewq_app::reports::{
    FindingBasis, FindingBasisKind, FindingDetails, FindingKind, FindingLocation, Severity,
};
use reviewq_domain::ledger::Tool;
use reviewq_domain::session::AbsolutePath;

use crate::SqliteStore;

mod companion;
mod managing;
mod passes;
mod publishing;
mod records;

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// The schema writes the tool names the domain records as literals, in the `ledger_calls`
/// check and the closing-call index, so they are compared here: the check lists exactly the
/// recorded tools, and the index selects `submit_review`.
#[test]
fn ledger_schema_names_the_recorded_tools() {
    let schema = include_str!("../schema/ledger.sql");
    let opening = "tool TEXT NOT NULL CHECK (tool IN (";
    let start = schema.find(opening).expect("the tool check") + opening.len();
    let end = start
        + schema[start..]
            .find("))")
            .expect("the end of the tool check");
    let listed: Vec<&str> = schema[start..end]
        .split(',')
        .map(|name| name.trim().trim_matches('\''))
        .collect();
    let recorded: Vec<&str> = Tool::ALL
        .iter()
        .filter(|tool| tool.is_recorded())
        .map(|tool| tool.as_str())
        .collect();
    assert_eq!(listed, recorded);
    let closing = format!(
        "ON ledger_calls(run_id, native_turn_id) WHERE tool = '{}';",
        Tool::SubmitReview.as_str()
    );
    assert!(schema.contains(&closing), "{closing}");
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
        kind: FindingKind::Functional,
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
            reply_to_note: None,
            details: None,
        },
    }
}

/// The settle read's `lead_idle` for each candidate run.
async fn lead_idle(store: &SqliteStore) -> Vec<bool> {
    let candidates = store.runs_to_settle().await.unwrap();
    candidates.iter().map(|run| run.facts.lead_idle).collect()
}

/// Stores the lead's observed activity, or `None` for unknown, as the host adapters do.
async fn activity(store: &SqliteStore, phase: impl Into<Option<&'static str>>) {
    let phase = phase.into();
    store
        .write(move |tx| crate::hosts::record_activity(&tx.sql, "claude:lead", phase, false))
        .await
        .unwrap();
}

/// The lead files a short note in each of `sections`, a flow note with its Mermaid diagram.
async fn file_notes(store: &SqliteStore, sections: &[NoteSection]) {
    for &section in sections {
        let body = match section {
            NoteSection::Flow => {
                "```mermaid\nflowchart LR\n  Retry --> Ledger\n```\n\nA retry reads the stored key."
            }
            _ => "Retries reuse the first key.",
        };
        let note = NoteWrite {
            key: NoteKey::fixed(section).unwrap(),
            body: body.into(),
            expected_revision: None,
        };
        let write = RecordWrite::WriteNote(note);
        let id = format!("note-{section}");
        store.write_record(call(&id), write).await.unwrap().unwrap();
    }
}
