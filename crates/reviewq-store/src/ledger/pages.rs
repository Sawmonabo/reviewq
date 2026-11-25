//! `review_ledger`: one page of a run's ledger, holding whole records up to the page budget.

use reviewq_app::access::{AppError, FieldError};
use reviewq_app::ledger::{
    FindingRecord, HostSession, LedgerCursor, LedgerEntries, LedgerPage, LedgerRead,
    LedgerSelection, MAX_LEDGER_PAGE_BYTES, NoteKey, NoteLine, NoteRecord, NoteSection,
};
use reviewq_domain::identity::RunId;
use reviewq_domain::ledger::{FindingId, RecordRevision, Tool, may_call};
use rusqlite::{Connection, OptionalExtension, params};

use super::rows::{self, FINDING_COLUMNS, NOTE_COLUMNS, section_order};
use super::{caller, passes, sequence};
use crate::StoreError;
use crate::queue::{parse, text};

pub(super) fn read(
    db: &Connection,
    session: &HostSession,
    read: LedgerRead,
) -> Result<LedgerPage, StoreError> {
    let (lead, agent) = caller::attribute(db, session)?;
    if !may_call(Tool::ReviewLedger, agent.role()) {
        return Err(AppError::Forbidden.into());
    }
    let run = lead.run_id();
    let sequence = sequence(db, run.as_str())?;
    let LedgerRead { selection, after } = read;
    let start = match after {
        Some(cursor) => {
            if &cursor.run != run || cursor.selection != selection {
                return Err(AppError::InvalidFields(vec![FieldError::new(
                    "/cursor",
                    "This cursor belongs to another read. Send it with the selection it was issued for.",
                )])
                .into());
            }
            if cursor.sequence != sequence {
                return Err(AppError::CursorExpired.into());
            }
            usize::try_from(cursor.position).map_err(|_| AppError::Internal)?
        }
        None => 0,
    };
    let (entries, end, total) = match &selection {
        LedgerSelection::Short => short(db, run, start)?,
        LedgerSelection::Findings(ids) => findings(db, run, ids, start)?,
        LedgerSelection::Notes(sections) => notes(db, run, sections, start)?,
    };
    let next = (end < total)
        .then(|| {
            Ok::<_, StoreError>(LedgerCursor {
                run: run.clone(),
                selection,
                sequence,
                position: u32::try_from(end).map_err(|_| AppError::Internal)?,
            })
        })
        .transpose()?;
    let receipt: Option<String> = db
        .prepare_cached("SELECT receipt_id FROM report_heads WHERE run_id=?1")?
        .query_row([run.as_str()], |row| row.get(0))
        .optional()?;
    Ok(LedgerPage {
        receipt: receipt.as_deref().map(parse).transpose()?,
        pass: passes::open_pass(db, run.as_str())?,
        entries,
        next,
    })
}

/// How many of `sizes`, from `start`, fit the page budget: at least one, while any remain.
fn window(sizes: &[usize], start: usize) -> usize {
    let mut used = 0;
    let mut end = start;
    for size in sizes.iter().skip(start) {
        if end > start && used + size > MAX_LEDGER_PAGE_BYTES {
            break;
        }
        used += size;
        end += 1;
    }
    end
}

type Page = (LedgerEntries, usize, usize);

fn short(db: &Connection, run: &RunId, start: usize) -> Result<Page, StoreError> {
    let lines = passes::lines(db, run, "f.finding")?;
    let mut notes = note_index(db, run, None)?;
    notes.sort_by_key(|note| note.order);
    let sizes: Vec<usize> = lines
        .iter()
        .map(|line| line.title.len())
        .chain(notes.iter().map(|note| note.title_bytes))
        .collect();
    let end = window(&sizes, start);
    let total = sizes.len();
    let split = lines.len();
    let findings = lines
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect::<Vec<_>>();
    let notes = notes
        .into_iter()
        .enumerate()
        .filter(|(index, _)| (start..end).contains(&(split + index)))
        .map(|(_, note)| NoteLine {
            key: note.key,
            revision: note.revision,
            characters: note.characters,
        })
        .collect();
    Ok((LedgerEntries::Short { findings, notes }, end, total))
}

fn findings(
    db: &Connection,
    run: &RunId,
    ids: &[FindingId],
    start: usize,
) -> Result<Page, StoreError> {
    let sql = format!(
        "SELECT {FINDING_COLUMNS} FROM ledger_findings f
         JOIN ledger_changes c ON c.run_id=f.run_id AND c.sequence=f.sequence
         WHERE f.run_id=?1 AND f.finding=?2"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut errors = Vec::new();
    let mut records: Vec<FindingRecord> = Vec::with_capacity(ids.len());
    for (index, id) in ids.iter().enumerate() {
        let mut found = statement.query(params![run.as_str(), id.number()])?;
        match found.next()? {
            Some(row) => {
                let record = rows::finding(row, 0)?;
                if !records.iter().any(|known| known.id == record.id) {
                    records.push(record);
                }
            }
            None => errors.push(FieldError::new(
                format!("/ids/{index}"),
                format!("{id} is not a finding of this run."),
            )),
        }
    }
    if !errors.is_empty() {
        return Err(AppError::InvalidFields(errors).into());
    }
    records.sort_by_key(|record| record.id.number());
    let sizes: Vec<usize> = records.iter().map(finding_bytes).collect();
    let end = window(&sizes, start);
    let findings = records
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    let entries = LedgerEntries::Full {
        findings,
        notes: Vec::new(),
    };
    Ok((entries, end, sizes.len()))
}

fn notes(
    db: &Connection,
    run: &RunId,
    sections: &[NoteSection],
    start: usize,
) -> Result<Page, StoreError> {
    let mut index = note_index(db, run, Some(sections))?;
    index.sort_by_key(|note| note.order);
    let sizes: Vec<usize> = index
        .iter()
        .map(|note| note.title_bytes + note.body_bytes)
        .collect();
    let end = window(&sizes, start);
    let sql =
        format!("SELECT {NOTE_COLUMNS} FROM ledger_changes c WHERE c.run_id=?1 AND c.sequence=?2");
    let mut statement = db.prepare_cached(&sql)?;
    let mut notes: Vec<NoteRecord> = Vec::new();
    for note in index.iter().skip(start).take(end.saturating_sub(start)) {
        let mut found = statement.query(params![run.as_str(), note.sequence])?;
        let row = found.next()?.ok_or(AppError::Internal)?;
        notes.push(rows::note(row, 0)?);
    }
    let entries = LedgerEntries::Full {
        findings: Vec::new(),
        notes,
    };
    Ok((entries, end, sizes.len()))
}

/// A present note without its body.
struct NoteIndex {
    key: NoteKey,
    revision: RecordRevision,
    sequence: i64,
    order: (u8, i64),
    title_bytes: usize,
    body_bytes: usize,
    characters: u32,
}

fn note_index(
    db: &Connection,
    run: &RunId,
    sections: Option<&[NoteSection]>,
) -> Result<Vec<NoteIndex>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT n.section, n.title, n.revision, n.sequence, n.first_sequence,
           length(CAST(c.body AS BLOB)), length(c.body)
         FROM ledger_notes n JOIN ledger_changes c ON c.run_id=n.run_id AND c.sequence=n.sequence
         WHERE n.run_id=?1 AND n.present=1",
    )?;
    let mut found = statement.query([run.as_str()])?;
    let mut notes = Vec::new();
    while let Some(row) = found.next()? {
        let key = rows::note_key(text(row, 0)?, text(row, 1)?)?;
        if sections.is_some_and(|sections| !sections.contains(&key.section())) {
            continue;
        }
        let count = |column| -> Result<i64, StoreError> { Ok(row.get(column)?) };
        notes.push(NoteIndex {
            order: (section_order(key.section()), count(4)?),
            title_bytes: key.title().map_or(0, str::len),
            revision: rows::revision(count(2)?)?,
            sequence: count(3)?,
            body_bytes: usize::try_from(count(5)?).map_err(|_| AppError::Internal)?,
            characters: u32::try_from(count(6)?).map_err(|_| AppError::Internal)?,
            key,
        });
    }
    Ok(notes)
}

/// The UTF-8 bytes of a finding's text fields, which the page budget counts.
fn finding_bytes(record: &FindingRecord) -> usize {
    let fields = &record.fields;
    let body = &fields.body;
    let outcome = record.outcome.as_ref().map_or(0, |outcome| {
        outcome.reason.len()
            + outcome
                .discussion_ids
                .iter()
                .map(String::len)
                .sum::<usize>()
    });
    fields.title.len()
        + fields.category.as_ref().map_or(0, String::len)
        + body
            .locations
            .iter()
            .map(|location| location.path.len())
            .sum::<usize>()
        + body.issue.len()
        + body.consequence.len()
        + body.evidence.text.len()
        + body.proposed_comment.as_ref().map_or(0, String::len)
        + body.details.as_ref().map_or(0, String::len)
        + outcome
}
