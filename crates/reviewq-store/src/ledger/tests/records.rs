//! The lead's record writes: replays, revisions and the reply a comment names.

use std::os::unix::fs::PermissionsExt;

use reviewq_app::ledger::{
    FindingPatch, LedgerEntries, LedgerRead, LedgerSelection, LedgerStore, NoteSection,
    RecordField, RecordReceipt, RecordWrite, WriteRefusal,
};
use reviewq_app::reports::Severity;
use reviewq_domain::ledger::{FindingId, PassKind, RecordRevision};
use reviewq_domain::session::AbsolutePath;

use super::{call, fields, file_notes, lead, open, run, short};
use crate::SqliteStore;

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
        // A comment and the note it replies to save in one call, a change of the note alone
        // makes a revision, and removing the comment removes the note.
        let reply = |note: &str| FindingPatch {
            reply_to_note: Some(Some(note.into())),
            ..FindingPatch::default()
        };
        let write = |name: &'static str, at: RecordRevision, patch| {
            store.write_record(call(name), change(at, patch))
        };
        let commented = FindingPatch {
            proposed_comment: Some(Some("Reuse the stored key.".into())),
            ..reply("9001")
        };
        let second = RecordRevision::FIRST.next().unwrap();
        let third = second.next().unwrap();
        for (name, at, patch) in [
            ("comment", RecordRevision::FIRST, commented),
            ("reply", second, reply("9002")),
        ] {
            let changed = write(name, at, patch).await.unwrap().unwrap();
            let next = at.next().unwrap();
            assert!(
                matches!(changed.receipt, RecordReceipt::Finding { revision, .. } if revision == next)
            );
        }
        let reseverity = FindingPatch {
            severity: Some(Severity::P2),
            ..FindingPatch::default()
        };
        let stale = write("stale", second, reseverity).await.unwrap();
        assert_eq!(
            stale,
            Err(WriteRefusal::StaleRevision {
                current: third,
                changed: vec![RecordField::ReplyToNote],
            })
        );
        let uncomment = FindingPatch {
            proposed_comment: Some(None),
            ..FindingPatch::default()
        };
        write("uncomment", third, uncomment).await.unwrap().unwrap();
        let fourth = third.next().unwrap();
        let alone = write("alone", fourth, reply("9003")).await.unwrap();
        assert_eq!(alone, Err(WriteRefusal::ReplyWithoutComment));
        // A new finding that names a note but has no comment is refused the same way, before
        // the schema's check, and saves nothing: the short read below lists F-01 alone.
        let mut orphan = fields();
        orphan.title = "Refund skips the ledger lock".into();
        orphan.body.reply_to_note = Some("9001".into());
        let create = RecordWrite::CreateFinding(Box::new(orphan));
        let orphan = store.write_record(call("orphan"), create).await.unwrap();
        assert_eq!(orphan, Err(WriteRefusal::ReplyWithoutComment));
        let full = LedgerRead {
            selection: LedgerSelection::Findings(vec![id]),
            after: None,
        };
        let LedgerEntries::Full { findings, .. } =
            store.read_ledger(lead(), full).await.unwrap().entries
        else {
            panic!("a read by ID returns records");
        };
        let body = &findings[0].fields.body;
        assert_eq!(
            (&body.proposed_comment, &body.reply_to_note),
            (&None, &None)
        );
        file_notes(&store, &[NoteSection::Summary]).await;
        store.close(SqliteStore::CLOSE_DEADLINE).await.unwrap();

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
            [(id, fourth)]
        );
        assert_eq!(notes.len(), 1);
        assert_eq!(page.pass.map(|pass| pass.kind), Some(PassKind::FullReview));
        store.close(SqliteStore::CLOSE_DEADLINE).await.unwrap();
    });
}

fn change(expected: RecordRevision, patch: FindingPatch) -> RecordWrite {
    RecordWrite::ChangeFinding {
        id: FindingId::FIRST,
        expected_revision: expected,
        patch: Box::new(patch),
    }
}
