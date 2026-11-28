//! `write_record`: one immediate transaction that replays a known call, applies the duplicate
//! rules, the expected revision and the per-run limits, then writes the record, its change row,
//! the run's ledger sequence, a pass when one opens, the lead's activity when its own call
//! makes a stored idle state stale, and the call's result.

use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::ledger::{
    Author, CallBinding, Caller, FindingAssessment, FindingFields, FindingPatch, FindingRecord,
    MAX_ACTIVE_FINDINGS, MAX_EXTRA_NOTES, NoteKey, NoteSection, NoteWrite, RecordField,
    RecordReceipt, RecordWrite, Recorded, WriteRefusal,
};
use reviewq_domain::identity::Revision;
use reviewq_domain::ledger::{CallerRole, FindingId, RecordRevision, may_call, record_opens_pass};
use rusqlite::{Connection, OptionalExtension, params};

use super::calls::{self, NEW_FINDING, finding_target, note_target};
use super::rows::{
    self, FINDING_COLUMNS, NOTE_COLUMNS, finding_changes, locations_json, note_title,
};
use super::{active_findings, caller, passes, unpublished};
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::workspace::{advance_queue, provider_label};

/// A write that passed every rule: the record to store, or the stored one when the write
/// equals it.
enum Planned {
    Finding {
        record: Box<FindingRecord>,
        /// The outcome is new with this write, so its sequence is this change's.
        assessed: bool,
        changed: bool,
    },
    Note {
        key: NoteKey,
        revision: RecordRevision,
        body: String,
        changed: bool,
    },
}

pub(super) fn write(
    tx: &mut WriteTx<'_>,
    binding: &CallBinding,
    write: RecordWrite,
) -> Result<Result<Recorded, WriteRefusal>, StoreError> {
    let db = &tx.sql;
    let caller = caller::resolve(db, binding)?;
    let tool = write.tool();
    if !may_call(tool, caller.role()) {
        return Err(AppError::Forbidden.into());
    }
    let run = caller.lead.run_id().clone();
    let target = match &write {
        RecordWrite::CreateFinding(_) => NEW_FINDING.to_owned(),
        RecordWrite::ChangeFinding { id, .. } => finding_target(*id),
        RecordWrite::SetFindingOutcome(assessment) => finding_target(assessment.id),
        RecordWrite::WriteNote(note) => note_target(&note.key),
    };
    if let Some((stored, _)) = calls::replay(db, &caller, tool, &target)? {
        let receipt = calls::receipt_from(&stored)?;
        return Ok(Ok(Recorded { run, receipt }));
    }
    let planned = match write {
        RecordWrite::CreateFinding(fields) => create(db, &caller, *fields)?,
        RecordWrite::ChangeFinding {
            id,
            expected_revision,
            patch,
        } => change(db, run.as_str(), id, expected_revision, *patch)?,
        RecordWrite::SetFindingOutcome(assessment) => assess(db, run.as_str(), assessment)?,
        RecordWrite::WriteNote(note) => write_note(db, run.as_str(), note)?,
    };
    let planned = match planned {
        Ok(planned) => planned,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let time = now_ms()?;
    let mut activity_changed = false;
    if caller.role() == CallerRole::Lead {
        passes::lead_seen(db, run.as_str(), caller.turn.as_ref(), time)?;
        activity_changed = passes::lead_calling(db, run.as_str())?;
    }
    let changed = match &planned {
        Planned::Finding { changed, .. } | Planned::Note { changed, .. } => *changed,
    };
    let sequence = if changed {
        let sequence = advance(db, run.as_str())?;
        match &planned {
            Planned::Finding {
                record, assessed, ..
            } => put_finding(db, &caller, sequence, record, *assessed, time)?,
            Planned::Note {
                key,
                revision,
                body,
                ..
            } => put_note(db, &caller, sequence, key, *revision, body, time)?,
        }
        open_pass(db, &caller, sequence, time)?;
        sequence
    } else {
        super::sequence(db, run.as_str())?
    };
    let unpublished = unpublished(db, run.as_str())?;
    let receipt = match planned {
        Planned::Finding { record, .. } => RecordReceipt::Finding {
            id: record.id,
            revision: record.revision,
            unpublished,
        },
        Planned::Note { key, revision, .. } => RecordReceipt::Note {
            key,
            revision,
            unpublished,
        },
    };
    let result = calls::receipt_json(&receipt);
    calls::store(db, &caller, tool, &target, sequence, &result)?;
    if activity_changed {
        advance_queue(&tx.sql)?;
        tx.emit(&[ResourceKey::Queue, ResourceKey::Run(Some(run.clone()))])?;
    }
    Ok(Ok(Recorded { run, receipt }))
}

fn create(
    db: &Connection,
    caller: &Caller,
    fields: FindingFields,
) -> Result<Result<Planned, WriteRefusal>, StoreError> {
    if replies_without_comment(&fields) {
        return Ok(Err(WriteRefusal::ReplyWithoutComment));
    }
    let run = caller.lead.run_id().as_str();
    let first = fields.body.locations.first();
    let matched: Option<i64> = db
        .prepare_cached(
            "SELECT finding FROM ledger_findings
             WHERE run_id=?1 AND active=1 AND title=?2 AND first_path IS ?3 AND first_line IS ?4
             ORDER BY finding LIMIT 1",
        )?
        .query_row(
            params![
                run,
                fields.title,
                first.map(|location| location.path.as_str()),
                first.and_then(|location| location.line)
            ],
            |row| row.get(0),
        )
        .optional()?;
    let matched = match matched {
        Some(id) => current_finding(db, run, rows::finding_id(id)?)?.map(|(record, _)| record),
        None => None,
    };
    if let Some(record) = matched {
        if record.fields != fields {
            return Ok(Err(WriteRefusal::DuplicateFinding { id: record.id }));
        }
        return Ok(Ok(Planned::Finding {
            record: Box::new(record),
            assessed: false,
            changed: false,
        }));
    }
    if active_findings(db, run)? >= MAX_ACTIVE_FINDINGS {
        return Ok(Err(WriteRefusal::TooManyFindings));
    }
    let next: i64 = db
        .prepare_cached(
            "SELECT coalesce(max(finding), 0) + 1 FROM ledger_findings WHERE run_id=?1",
        )?
        .query_row([run], |row| row.get(0))?;
    let id = rows::finding_id(next).map_err(|_| AppError::Conflict)?;
    Ok(Ok(Planned::Finding {
        record: Box::new(FindingRecord {
            id,
            revision: RecordRevision::FIRST,
            author: Author::of(caller),
            fields,
            outcome: None,
        }),
        assessed: false,
        changed: true,
    }))
}

fn change(
    db: &Connection,
    run: &str,
    id: FindingId,
    expected: RecordRevision,
    patch: FindingPatch,
) -> Result<Result<Planned, WriteRefusal>, StoreError> {
    let Some((mut record, active)) = current_finding(db, run, id)? else {
        return Ok(Err(WriteRefusal::UnknownFinding));
    };
    if !patch_changes(&record.fields, &patch) {
        return Ok(Ok(Planned::Finding {
            record: Box::new(record),
            assessed: false,
            changed: false,
        }));
    }
    if let Some(refusal) = stale_finding(db, run, &record, expected)? {
        return Ok(Err(refusal));
    }
    apply(&mut record.fields, patch);
    if replies_without_comment(&record.fields) {
        return Ok(Err(WriteRefusal::ReplyWithoutComment));
    }
    if !active && active_findings(db, run)? >= MAX_ACTIVE_FINDINGS {
        return Ok(Err(WriteRefusal::TooManyFindings));
    }
    if !active {
        record.outcome = None;
    }
    record.revision = record.revision.next().ok_or(AppError::Conflict)?;
    Ok(Ok(Planned::Finding {
        record: Box::new(record),
        assessed: false,
        changed: true,
    }))
}

fn assess(
    db: &Connection,
    run: &str,
    assessment: FindingAssessment,
) -> Result<Result<Planned, WriteRefusal>, StoreError> {
    let Some((mut record, active)) = current_finding(db, run, assessment.id)? else {
        return Ok(Err(WriteRefusal::UnknownFinding));
    };
    if record.outcome.as_ref() == Some(&assessment.outcome) {
        return Ok(Ok(Planned::Finding {
            record: Box::new(record),
            assessed: false,
            changed: false,
        }));
    }
    if let Some(refusal) = stale_finding(db, run, &record, assessment.expected_revision)? {
        return Ok(Err(refusal));
    }
    let reopens = !active && !assessment.outcome.conclusion.retires_finding();
    if reopens && active_findings(db, run)? >= MAX_ACTIVE_FINDINGS {
        return Ok(Err(WriteRefusal::TooManyFindings));
    }
    record.outcome = Some(assessment.outcome);
    record.revision = record.revision.next().ok_or(AppError::Conflict)?;
    Ok(Ok(Planned::Finding {
        record: Box::new(record),
        assessed: true,
        changed: true,
    }))
}

fn write_note(
    db: &Connection,
    run: &str,
    note: NoteWrite,
) -> Result<Result<Planned, WriteRefusal>, StoreError> {
    let NoteWrite {
        key,
        body,
        expected_revision,
    } = note;
    let section = key.section().to_string();
    let title = note_title(&key);
    let sql = format!(
        "SELECT n.present, {NOTE_COLUMNS} FROM ledger_notes n
         JOIN ledger_changes c ON c.run_id=n.run_id AND c.sequence=n.sequence
         WHERE n.run_id=?1 AND n.section=?2 AND n.title=?3"
    );
    let stored = {
        let mut statement = db.prepare_cached(&sql)?;
        let mut found = statement.query(params![run, section, title])?;
        match found.next()? {
            Some(row) => Some((row.get::<_, bool>(0)?, rows::note(row, 1)?)),
            None => None,
        }
    };
    let present = stored.as_ref().is_some_and(|(present, _)| *present);
    if body.is_empty() && stored.is_none() {
        return Ok(Err(WriteRefusal::UnknownNote));
    }
    // An equal write, including the removal of a removed note, returns the current receipt.
    if let Some((present, current)) = &stored
        && (if *present {
            current.body == body
        } else {
            body.is_empty()
        })
    {
        return Ok(Ok(Planned::Note {
            revision: current.revision,
            key,
            body,
            changed: false,
        }));
    }
    if let Some((_, current)) = &stored {
        match expected_revision {
            Some(expected) if expected != current.revision => {
                let earlier: Option<String> = db
                    .prepare_cached(
                        "SELECT body FROM ledger_changes
                         WHERE run_id=?1 AND note_section=?2 AND note_title=?3 AND revision=?4",
                    )?
                    .query_row(
                        params![run, section, title, expected.as_sqlite_integer()],
                        |row| row.get(0),
                    )
                    .optional()?;
                let changed = earlier
                    .is_some_and(|earlier| earlier != current.body)
                    .then_some(RecordField::Body)
                    .into_iter()
                    .collect();
                return Ok(Err(WriteRefusal::StaleRevision {
                    current: current.revision,
                    changed,
                }));
            }
            None if present => {
                return Ok(Err(WriteRefusal::RevisionRequired {
                    current: current.revision,
                }));
            }
            _ => {}
        }
    }
    if key.section() == NoteSection::Extra && !present && extra_notes(db, run)? >= MAX_EXTRA_NOTES {
        return Ok(Err(WriteRefusal::TooManyExtraNotes));
    }
    let revision = match &stored {
        Some((_, current)) => current.revision.next().ok_or(AppError::Conflict)?,
        None => RecordRevision::FIRST,
    };
    Ok(Ok(Planned::Note {
        key,
        revision,
        body,
        changed: true,
    }))
}

/// The finding at its current revision and whether it is active.
fn current_finding(
    db: &Connection,
    run: &str,
    id: FindingId,
) -> Result<Option<(FindingRecord, bool)>, StoreError> {
    let sql = format!(
        "SELECT f.active, {FINDING_COLUMNS} FROM ledger_findings f
         JOIN ledger_changes c ON c.run_id=f.run_id AND c.sequence=f.sequence
         WHERE f.run_id=?1 AND f.finding=?2"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query(params![run, id.number()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some((rows::finding(row, 1)?, row.get(0)?)))
}

/// `StaleRevision` naming the fields that differ between `expected` and `current`, unless
/// `expected` is current.
fn stale_finding(
    db: &Connection,
    run: &str,
    current: &FindingRecord,
    expected: RecordRevision,
) -> Result<Option<WriteRefusal>, StoreError> {
    if expected == current.revision {
        return Ok(None);
    }
    let sql = format!(
        "SELECT {FINDING_COLUMNS} FROM ledger_changes c
         WHERE c.run_id=?1 AND c.finding=?2 AND c.revision=?3"
    );
    let earlier = db
        .prepare_cached(&sql)?
        .query_row(
            params![run, current.id.number(), expected.as_sqlite_integer()],
            |row| Ok(rows::finding(row, 0)),
        )
        .optional()?
        .transpose()?;
    Ok(Some(WriteRefusal::StaleRevision {
        current: current.revision,
        changed: earlier.map_or_else(Vec::new, |earlier| finding_changes(&earlier, current)),
    }))
}

/// Whether any field `patch` gives differs from `fields`.
fn patch_changes(fields: &FindingFields, patch: &FindingPatch) -> bool {
    fn differs<T: PartialEq>(given: Option<&T>, stored: &T) -> bool {
        given.is_some_and(|given| given != stored)
    }
    let body = &fields.body;
    differs(patch.title.as_ref(), &fields.title)
        || differs(patch.severity.as_ref(), &fields.severity)
        || differs(patch.blocking.as_ref(), &fields.blocking)
        || differs(patch.kind.as_ref(), &fields.kind)
        || patch
            .category
            .as_ref()
            .is_some_and(|category| fields.category.as_ref() != Some(category))
        || differs(patch.locations.as_ref(), &body.locations)
        || differs(patch.issue.as_ref(), &body.issue)
        || differs(patch.consequence.as_ref(), &body.consequence)
        || differs(patch.evidence.as_ref(), &body.evidence)
        || differs(patch.proposed_comment.as_ref(), &body.proposed_comment)
        || differs(patch.reply_to_note.as_ref(), &body.reply_to_note)
        || differs(patch.details.as_ref(), &body.details)
}

/// Whether `fields` name a note for the proposed comment to reply to without a proposed
/// comment. The one rule of the reply target: it exists only with a comment, so a write that
/// would leave one alone is refused, and `apply` removes it with the comment.
fn replies_without_comment(fields: &FindingFields) -> bool {
    fields.body.reply_to_note.is_some() && fields.body.proposed_comment.is_none()
}

/// Stores each field `patch` gives. Removing the proposed comment also removes the reply
/// target unless the patch names one, which `replies_without_comment` then refuses.
fn apply(fields: &mut FindingFields, patch: FindingPatch) {
    let body = &mut fields.body;
    let FindingPatch {
        title,
        severity,
        blocking,
        kind,
        category,
        locations,
        issue,
        consequence,
        evidence,
        proposed_comment,
        reply_to_note,
        details,
    } = patch;
    if let Some(value) = title {
        fields.title = value;
    }
    if let Some(value) = severity {
        fields.severity = value;
    }
    if let Some(value) = blocking {
        fields.blocking = value;
    }
    if let Some(value) = kind {
        fields.kind = value;
    }
    if let Some(value) = category {
        fields.category = Some(value);
    }
    if let Some(value) = locations {
        body.locations = value;
    }
    if let Some(value) = issue {
        body.issue = value;
    }
    if let Some(value) = consequence {
        body.consequence = value;
    }
    if let Some(value) = evidence {
        body.evidence = value;
    }
    if let Some(value) = proposed_comment {
        if value.is_none() && reply_to_note.is_none() {
            body.reply_to_note = None;
        }
        body.proposed_comment = value;
    }
    if let Some(value) = reply_to_note {
        body.reply_to_note = value;
    }
    if let Some(value) = details {
        body.details = value;
    }
}

fn extra_notes(db: &Connection, run: &str) -> Result<u32, StoreError> {
    super::count(
        db,
        "SELECT count(*) FROM ledger_notes WHERE run_id=?1 AND section='extra' AND present=1",
        run,
    )
}

/// Advances the run's ledger sequence and returns the new value.
fn advance(db: &Connection, run: &str) -> Result<Revision, StoreError> {
    let value: i64 = db
        .prepare_cached(
            "INSERT INTO ledger_runs(run_id, sequence) VALUES(?1, 1)
             ON CONFLICT(run_id) DO UPDATE SET sequence=sequence+1 RETURNING sequence",
        )?
        .query_row([run], |row| row.get(0))?;
    Revision::try_from(value).map_err(|_| AppError::Internal.into())
}

/// The change row's call identity: provider, native session, turn, call and author.
fn author(caller: &Caller) -> Option<String> {
    match Author::of(caller) {
        Author::Lead => None,
        Author::Subagent(agent) => Some(agent),
    }
}

fn put_finding(
    db: &Connection,
    caller: &Caller,
    sequence: Revision,
    record: &FindingRecord,
    assessed: bool,
    time: i64,
) -> Result<(), StoreError> {
    let run = caller.lead.run_id().as_str();
    let fields = &record.fields;
    let body = &fields.body;
    let outcome = record.outcome.as_ref();
    let discussions = outcome
        .map(|outcome| serde_json::to_string(&outcome.discussion_ids))
        .transpose()
        .map_err(|_| AppError::Internal)?;
    db.prepare_cached(
        "INSERT INTO ledger_changes(run_id, sequence, finding, revision, provider, native_session,
           native_turn_id, call_id, author, changed_at_ms, severity, blocking, kind, title,
           category, evidence_kind, conclusion, checked_sha, outcome_reason, discussion_ids,
           locations, issue, consequence, evidence, proposed_comment, reply_to_note, details)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,
           ?23,?24,?25,?26,?27)",
    )?
    .execute(params![
        run,
        sequence.as_sqlite_integer(),
        record.id.number(),
        record.revision.as_sqlite_integer(),
        provider_label(caller.provider()),
        caller.native_session().as_str(),
        caller.turn.as_ref().map(|turn| turn.as_str()),
        caller.call_id,
        author(caller),
        time,
        fields.severity.to_string(),
        fields.blocking,
        fields.kind.to_string(),
        fields.title,
        fields.category,
        body.evidence.kind.to_string(),
        outcome.map(|outcome| outcome.conclusion.to_string()),
        outcome.and_then(|outcome| outcome.checked_sha.as_ref().map(|sha| sha.as_str())),
        outcome.map(|outcome| outcome.reason.as_str()),
        discussions,
        locations_json(&body.locations),
        body.issue,
        body.consequence,
        body.evidence.text,
        body.proposed_comment,
        body.reply_to_note,
        body.details,
    ])?;
    let first = body.locations.first();
    db.prepare_cached(
        "INSERT INTO ledger_findings(run_id, finding, revision, sequence, outcome_sequence, active,
           title, first_path, first_line)
         VALUES(?1, ?2, ?3, ?4, CASE WHEN ?5 THEN ?4 END, ?6, ?7, ?8, ?9)
         ON CONFLICT(run_id, finding) DO UPDATE SET revision=excluded.revision,
           sequence=excluded.sequence,
           outcome_sequence=CASE WHEN ?5 THEN ?4 WHEN ?10 THEN ledger_findings.outcome_sequence END,
           active=excluded.active, title=excluded.title, first_path=excluded.first_path,
           first_line=excluded.first_line",
    )?
    .execute(params![
        run,
        record.id.number(),
        record.revision.as_sqlite_integer(),
        sequence.as_sqlite_integer(),
        assessed,
        record.is_active(),
        fields.title,
        first.map(|location| location.path.as_str()),
        first.and_then(|location| location.line),
        outcome.is_some(),
    ])?;
    Ok(())
}

fn put_note(
    db: &Connection,
    caller: &Caller,
    sequence: Revision,
    key: &NoteKey,
    revision: RecordRevision,
    body: &str,
    time: i64,
) -> Result<(), StoreError> {
    let run = caller.lead.run_id().as_str();
    let section = key.section().to_string();
    let title = note_title(key);
    db.prepare_cached(
        "INSERT INTO ledger_changes(run_id, sequence, note_section, note_title, revision, provider,
           native_session, native_turn_id, call_id, author, changed_at_ms, body)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?
    .execute(params![
        run,
        sequence.as_sqlite_integer(),
        section,
        title,
        revision.as_sqlite_integer(),
        provider_label(caller.provider()),
        caller.native_session().as_str(),
        caller.turn.as_ref().map(|turn| turn.as_str()),
        caller.call_id,
        author(caller),
        time,
        body,
    ])?;
    db.prepare_cached(
        "INSERT INTO ledger_notes(run_id, section, title, revision, sequence, first_sequence, present)
         VALUES(?1, ?2, ?3, ?4, ?5, ?5, ?6)
         ON CONFLICT(run_id, section, title) DO UPDATE SET revision=excluded.revision,
           sequence=excluded.sequence, present=excluded.present",
    )?
    .execute(params![
        run,
        section,
        title,
        revision.as_sqlite_integer(),
        sequence.as_sqlite_integer(),
        !body.is_empty(),
    ])?;
    Ok(())
}

/// Opens a full-review pass for a record saved on a run with no version and no open pass.
fn open_pass(
    db: &Connection,
    caller: &Caller,
    sequence: Revision,
    time: i64,
) -> Result<(), StoreError> {
    let run = caller.lead.run_id().as_str();
    let has_version: bool = db
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM report_heads WHERE run_id=?1)")?
        .query_row([run], |row| row.get(0))?;
    let pass_open = passes::current_id(db, run)?.is_some();
    if record_opens_pass(has_version, pass_open) {
        let before = sequence.get().saturating_sub(1);
        passes::open_for_record(db, run, &caller.call_id, before, time)?;
    }
    Ok(())
}
