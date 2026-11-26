//! The worked snapshot composed and compared with the hand-written expected text.

use reviewq_domain::identity::MergeRequestIid;
use reviewq_domain::ledger::{FindingId, RecordRevision, VersionKind};

use super::*;
use crate::ledger::{Author, FindingRecord, NoteKey, ParentFinding, Span};
use crate::reports::{
    Coverage, DisplayIndex, FindingBasis, FindingConclusion, FindingDetails, FindingDisposition,
    FindingLocation, Publication, Verdict, build_report_display_index,
};

/// The expected text, written by hand from the spec's composed layout.
const WORKED: &str = include_str!("../../../../../fixtures/reviews/ledger/worked.md");
const REVIEWED: &str = "4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a";
const TARGET: &str = "9e8d7c6b9e8d7c6b9e8d7c6b9e8d7c6b9e8d7c6b";
const FIXED_AT: &str = "17a2b3c417a2b3c417a2b3c417a2b3c417a2b3c4";

fn sha(text: &str) -> GitSha {
    text.parse().expect("a full SHA")
}

fn revision(number: u64) -> RecordRevision {
    RecordRevision::try_from(number).expect("a revision")
}

fn location(path: &str, line: Option<u32>) -> FindingLocation {
    FindingLocation {
        path: path.to_owned(),
        line,
    }
}

fn evidence(kind: FindingBasisKind, text: &str) -> FindingBasis {
    FindingBasis {
        kind,
        text: text.to_owned(),
    }
}

/// A finding with its fields, on the parent version at `parent` with the lead's blocking
/// value when given.
fn finding(
    number: u32,
    at: u64,
    author: Author,
    fields: FindingFields,
    parent: Option<(u64, bool)>,
) -> SnapshotFinding {
    SnapshotFinding {
        record: FindingRecord {
            id: FindingId::new(number).expect("a finding number"),
            revision: revision(at),
            author,
            fields,
            outcome: None,
        },
        parent: parent.map(|(at, blocking)| ParentFinding {
            revision: revision(at),
            blocking,
        }),
        outcome_is_new: false,
        touched: false,
        by_subagent: false,
        disposition: None,
        discussion_ids: Vec::new(),
    }
}

fn outcome(
    conclusion: FindingConclusion,
    reason: &str,
    discussions: &[&str],
    checked: Option<&str>,
) -> Option<FindingOutcome> {
    Some(FindingOutcome {
        conclusion,
        reason: reason.to_owned(),
        discussion_ids: discussions.iter().map(|id| (*id).to_owned()).collect(),
        checked_sha: checked.map(sha),
    })
}

fn note(section: NoteSection, body: &str) -> NoteRecord {
    NoteRecord {
        key: NoteKey::fixed(section).expect("a fixed section"),
        revision: RecordRevision::FIRST,
        body: body.to_owned(),
    }
}

/// The worked snapshot: a lead's and a subagent's finding, a gap, two locations, a
/// retired finding with `|` in its reason, a keeping outcome on every other finding of the
/// parent version, one of them with no linked discussion, a user disposition that overrides
/// the lead's blocking value, every fixed note and one extra note, a title that starts with
/// code, a level-two heading inside details and a line with `Vec<u8>` and `<runner>`.
fn worked_findings() -> Vec<SnapshotFinding> {
    let mut retry_twice = finding(
        1,
        2,
        Author::Lead,
        FindingFields {
            title: "Retry writes the same invoice twice".to_owned(),
            severity: Severity::P1,
            blocking: true,
            section: FindingSection::Functional,
            category: None,
            body: FindingDetails {
                locations: vec![location("src/invoices.rs", Some(74))],
                issue: "The retry builds a fresh idempotency key after the first insert commits.".to_owned(),
                consequence: "A lost acknowledgement creates a duplicate invoice.".to_owned(),
                evidence: evidence(
                    FindingBasisKind::Observed,
                    "Replaying the request after a dropped reply inserted two rows.",
                ),
                proposed_comment: Some(
                    "Reuse the key stored with the first attempt instead of building a new one.\n"
                        .to_owned(),
                ),
                details: Some(
                    "\nThe timeline below shows the race.\n\n## Timeline\n\n1. The first insert commits.\n2. The reply is lost and the client retries.\n\nEach <runner> keeps the body as a Vec<u8> until the write commits.\n\n"
                        .to_owned(),
                ),
            },
        },
        Some((1, true)),
    );
    retry_twice.record.outcome = outcome(
        FindingConclusion::StillPresent,
        "The retry path still builds a new key.",
        &["D9"],
        None,
    );
    retry_twice.outcome_is_new = true;
    retry_twice.touched = true;
    retry_twice.discussion_ids = vec!["D9".to_owned()];

    let mut rebuilt_key = finding(
        2,
        2,
        Author::Subagent("agent-7".to_owned()),
        FindingFields {
            title: "`retry_key` is rebuilt on every attempt".to_owned(),
            severity: Severity::P2,
            blocking: true,
            section: FindingSection::Quality,
            category: Some("Reliability".to_owned()),
            body: FindingDetails {
                locations: vec![
                    location("src/retry.rs", Some(12)),
                    location("docs/retry`notes.md", None),
                ],
                issue: "`retry_key` is called inside the loop, so each attempt gets a new key."
                    .to_owned(),
                consequence: "The server cannot tell a retry from a new request.".to_owned(),
                evidence: evidence(
                    FindingBasisKind::Observed,
                    "The unit test `retries_reuse_key` fails at the reviewed commit.",
                ),
                proposed_comment: None,
                details: None,
            },
        },
        Some((1, true)),
    );
    // The full review assessed it, as it must every active finding of the parent version.
    rebuilt_key.record.outcome = outcome(
        FindingConclusion::StillPresent,
        "`retry_key` is still called inside the loop.",
        &[],
        None,
    );
    rebuilt_key.outcome_is_new = true;
    rebuilt_key.touched = true;
    rebuilt_key.by_subagent = true;
    rebuilt_key.disposition = Some(FindingDisposition {
        blocking: Some(false),
        publication: Some(Publication::Withheld),
        reason: "Agreed with the author\nto fix after the release.".to_owned(),
    });

    let mut separate_keys = finding(
        3,
        2,
        Author::Lead,
        FindingFields {
            title: "Two retry paths build separate keys".to_owned(),
            severity: Severity::P1,
            blocking: true,
            section: FindingSection::Functional,
            category: None,
            body: FindingDetails {
                locations: vec![location("src/retry.rs", Some(30))],
                issue: "The sync and async paths each build their own key.".to_owned(),
                consequence: "A retry on the other path writes a second invoice.".to_owned(),
                evidence: evidence(FindingBasisKind::Observed, "Both paths call `new_key`."),
                proposed_comment: None,
                details: None,
            },
        },
        Some((1, true)),
    );
    separate_keys.record.outcome = outcome(
        FindingConclusion::Fixed,
        "Both paths now share one key | verified by replay.",
        &["D7"],
        Some(FIXED_AT),
    );
    separate_keys.outcome_is_new = true;
    separate_keys.touched = true;

    let fixed_delay = finding(
        4,
        1,
        Author::Lead,
        FindingFields {
            title: "Retry delay is not configurable".to_owned(),
            severity: Severity::P3,
            blocking: false,
            section: FindingSection::Quality,
            category: None,
            body: FindingDetails {
                locations: Vec::new(),
                issue: "The delay between attempts is a constant.".to_owned(),
                consequence: "Operators cannot slow retries during an outage.".to_owned(),
                evidence: evidence(
                    FindingBasisKind::Gap,
                    "The production retry settings were not available to check.",
                ),
                proposed_comment: None,
                details: None,
            },
        },
        None,
    );
    vec![retry_twice, rebuilt_key, separate_keys, fixed_delay]
}

fn worked_notes() -> Vec<NoteRecord> {
    vec![
        note(
            NoteSection::Title,
            "Retry invoice writes without duplicates",
        ),
        note(
            NoteSection::Summary,
            "Adds a retry loop around invoice writes so a dropped acknowledgement no longer fails the request.\n\nThe retry reuses the request's idempotency key.",
        ),
        note(
            NoteSection::Rationale,
            "The retry still creates a new key on one path, which can write a duplicate invoice.",
        ),
        note(
            NoteSection::Flow,
            "1. `POST /invoices` reaches `InvoiceService::create`.\n2. The service writes the row and returns the receipt.",
        ),
        note(
            NoteSection::Changes,
            "- Adds `retry_write` in `src/retry.rs`.\n- Moves key creation into `src/invoices.rs`.",
        ),
        note(
            NoteSection::Threads,
            "The author replied on D7 that the shared key landed in the second commit.",
        ),
        note(
            NoteSection::Questions,
            "Should a retry give up after three attempts or after a time limit?",
        ),
        note(
            NoteSection::Verification,
            "Ran `cargo test -p invoices` at the reviewed commit. The replay check ran against a local database only.",
        ),
        note(NoteSection::Publication, "No comments are posted yet."),
        NoteRecord {
            key: NoteKey::extra("Rollout plan for *v2* & `beta` #".to_owned()),
            revision: RecordRevision::FIRST,
            body: "Ship behind the `invoice_retry` flag.\n".to_owned(),
        },
    ]
}

/// Where each line that starts with `prefix` begins in `WORKED`.
fn line_starts(prefix: &str) -> Vec<usize> {
    let mut offset = 0;
    let mut starts = Vec::new();
    for line in WORKED.split_inclusive('\n') {
        if line.starts_with(prefix) {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts
}

/// The byte range from `start` to the blank line before the next of `ends`, or to the
/// final newline.
fn block_from(start: usize, ends: &[usize]) -> std::ops::Range<usize> {
    let end = ends
        .iter()
        .copied()
        .find(|end| *end > start)
        .map_or(WORKED.len() - 1, |end| end - 2);
    start..end
}

fn assert_slices(span: &Span, expected: std::ops::Range<usize>, what: &str) {
    assert_eq!(span.bytes, expected, "{what}: bytes");
    assert_eq!(
        span.scalar_offset,
        WORKED[..expected.start].chars().count() as u64,
        "{what}: scalar offset"
    );
    assert_eq!(
        span.scalar_length,
        WORKED[expected].chars().count() as u64,
        "{what}: scalar length"
    );
}

#[test]
fn composed_worked_set_matches() {
    let cover = CoverFacts {
        project_path: "acme/billing".to_owned(),
        iid: MergeRequestIid::try_from(42_u64).expect("an MR IID"),
        mr_url: "https://gitlab.example.com/acme/billing/-/merge_requests/42".to_owned(),
        mr_title: "Retry invoice writes safely".to_owned(),
        target_sha: Some(sha(TARGET)),
        read_at_ms: 1_795_615_387_250,
    };
    let facts = VersionFacts {
        kind: VersionKind::FullReview,
        verdict: Verdict::RequestChanges,
        reviewed: RevisionEvidence::Exact(sha(REVIEWED)),
        coverage: Coverage::FullMr,
        coverage_source_sha: Some(sha(REVIEWED)),
        coverage_target_sha: Some(sha(TARGET)),
        full_mr_reviewed_sha: Some(sha(REVIEWED)),
        checked_at_ms: 1_795_615_387_250,
        new_attention: true,
    };
    let composed = compose(&cover, &facts, &worked_findings(), &worked_notes());
    assert_eq!(composed.text, WORKED);

    let sections = line_starts("## ");
    let cards = line_starts("### ");
    let ends: Vec<usize> = {
        let mut ends = [sections.as_slice(), cards.as_slice()].concat();
        ends.sort_unstable();
        ends
    };
    let titles: Vec<&str> = composed
        .sections
        .iter()
        .map(|section| section.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Principal code / data flow",
            "What changes across the MR",
            "Our previous threads",
            "Functional findings",
            "Quality findings",
            "Open questions and decisions",
            "Verification and limits",
            "Rollout plan for *v2* & `beta` #",
            "Publication and dispositions",
        ]
    );
    assert_eq!(composed.sections.len(), sections.len());
    for (section, start) in composed.sections.iter().zip(&sections) {
        assert_slices(&section.span, block_from(*start, &sections), &section.title);
    }
    let ids: Vec<String> = composed
        .cards
        .iter()
        .map(|card| card.id.to_string())
        .collect();
    assert_eq!(ids, ["F-01", "F-02", "F-04"]);
    assert_eq!(composed.cards.len(), cards.len());
    for (card, start) in composed.cards.iter().zip(&cards) {
        assert_slices(&card.span, block_from(*start, &ends), &card.id.to_string());
    }

    assert_eq!(composed.stats.byte_length, WORKED.len() as u64);
    assert_eq!(
        composed.stats.total_code_points,
        WORKED.chars().count() as u64
    );
    assert_eq!(composed.stats.line_count, WORKED.lines().count() as u64);
    assert_eq!(
        composed.stats.max_line_bytes,
        WORKED.lines().map(str::len).max().unwrap_or(0) as u64
    );
    assert!(
        matches!(
            build_report_display_index(&composed.text),
            DisplayIndex::Indexed(_)
        ),
        "inline HTML keeps the windowed reader"
    );
}
