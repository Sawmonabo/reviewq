//! One finding or note revision as `ledger_changes` stores it: the column lists, the decoders
//! that read them back and the field comparison a stale write reports.

use reviewq_app::access::AppError;
use reviewq_app::ledger::{
    Author, FindingFields, FindingLine, FindingOutcome, FindingRecord, NoteKey, NoteRecord,
    NoteSection, RecordField,
};
use reviewq_app::reports::{FindingBasis, FindingDetails, FindingLocation};
use reviewq_domain::ledger::{FindingId, RecordRevision};
use rusqlite::Row;
use serde_json::{Value, json};

use crate::StoreError;
use crate::queue::{parse, text};

/// The columns `finding` decodes, in order, from `ledger_changes c`.
pub(super) const FINDING_COLUMNS: &str = "c.finding, c.revision, c.author, c.title, c.severity,
    c.blocking, c.kind, c.category, c.locations, c.issue, c.consequence, c.evidence_kind,
    c.evidence, c.proposed_comment, c.reply_to_note, c.details, c.conclusion, c.outcome_reason,
    c.discussion_ids, c.checked_sha";

/// How many columns `FINDING_COLUMNS` names.
pub(super) const FINDING_WIDTH: usize = 20;

/// The columns `note` decodes, in order, from `ledger_changes c`.
pub(super) const NOTE_COLUMNS: &str = "c.note_section, c.note_title, c.revision, c.body";

/// The columns `line` decodes, in order, from `ledger_changes c`.
pub(super) const LINE_COLUMNS: &str =
    "c.finding, c.revision, c.severity, c.blocking, c.title, c.author";

/// Decodes `LINE_COLUMNS` starting at column 0.
pub(super) fn line(row: &Row<'_>) -> Result<FindingLine, StoreError> {
    let author: Option<String> = row.get(5)?;
    Ok(FindingLine {
        id: finding_id(row.get(0)?)?,
        revision: revision(row.get(1)?)?,
        severity: parse(text(row, 2)?)?,
        blocking: row.get(3)?,
        title: row.get(4)?,
        author: author.map_or(Author::Lead, Author::Subagent),
    })
}

/// Decodes `FINDING_COLUMNS` starting at column `at`.
pub(super) fn finding(row: &Row<'_>, at: usize) -> Result<FindingRecord, StoreError> {
    let author: Option<String> = row.get(at + 2)?;
    let conclusion: Option<String> = row.get(at + 16)?;
    let outcome = match conclusion {
        Some(conclusion) => Some(FindingOutcome {
            conclusion: parse(&conclusion)?,
            reason: row.get(at + 17)?,
            discussion_ids: from_json(text(row, at + 18)?)?,
            checked_sha: optional(row, at + 19)?,
        }),
        None => None,
    };
    Ok(FindingRecord {
        id: finding_id(row.get(at)?)?,
        revision: revision(row.get(at + 1)?)?,
        author: author.map_or(Author::Lead, Author::Subagent),
        fields: FindingFields {
            title: row.get(at + 3)?,
            severity: parse(text(row, at + 4)?)?,
            blocking: row.get(at + 5)?,
            kind: parse(text(row, at + 6)?)?,
            category: row.get(at + 7)?,
            body: details(row, at + 8)?,
        },
        outcome,
    })
}

/// Decodes the eight detail columns, `locations` through `details` in `FINDING_COLUMNS`
/// order, starting at column `at`.
fn details(row: &Row<'_>, at: usize) -> Result<FindingDetails, StoreError> {
    Ok(FindingDetails {
        locations: locations(text(row, at)?)?,
        issue: row.get(at + 1)?,
        consequence: row.get(at + 2)?,
        evidence: FindingBasis {
            kind: parse(text(row, at + 3)?)?,
            text: row.get(at + 4)?,
        },
        proposed_comment: row.get(at + 5)?,
        reply_to_note: row.get(at + 6)?,
        details: row.get(at + 7)?,
    })
}

/// Decodes `NOTE_COLUMNS` starting at column `at`.
pub(super) fn note(row: &Row<'_>, at: usize) -> Result<NoteRecord, StoreError> {
    Ok(NoteRecord {
        key: note_key(text(row, at)?, text(row, at + 1)?)?,
        revision: revision(row.get(at + 2)?)?,
        body: row.get(at + 3)?,
    })
}

pub(super) fn note_key(section: &str, title: &str) -> Result<NoteKey, StoreError> {
    let section: NoteSection = parse(section)?;
    if section == NoteSection::Extra {
        Ok(NoteKey::extra(title.to_owned()))
    } else {
        NoteKey::fixed(section).ok_or_else(|| AppError::Internal.into())
    }
}

/// The stored `note_title` of a note: an extra note's title, or empty for a fixed section.
pub(super) fn note_title(key: &NoteKey) -> &str {
    key.title().unwrap_or("")
}

/// Where a note sorts in a list: fixed sections in their declared order, then extra notes.
pub(super) fn section_order(section: NoteSection) -> u8 {
    match section {
        NoteSection::Title => 0,
        NoteSection::Summary => 1,
        NoteSection::Rationale => 2,
        NoteSection::Flow => 3,
        NoteSection::Changes => 4,
        NoteSection::Threads => 5,
        NoteSection::Questions => 6,
        NoteSection::Verification => 7,
        NoteSection::Publication => 8,
        NoteSection::Extra => 9,
    }
}

pub(super) fn locations_json(locations: &[FindingLocation]) -> String {
    Value::Array(
        locations
            .iter()
            .map(|location| json!({"path": location.path, "line": location.line}))
            .collect(),
    )
    .to_string()
}

/// Decodes a `locations` column, as `locations_json` writes it.
pub(crate) fn locations(source: &str) -> Result<Vec<FindingLocation>, StoreError> {
    let values: Vec<Value> = from_json(source)?;
    values
        .iter()
        .map(|value| {
            Ok(FindingLocation {
                path: value["path"].as_str().ok_or(AppError::Internal)?.to_owned(),
                line: match &value["line"] {
                    Value::Null => None,
                    line => Some(
                        line.as_u64()
                            .and_then(|line| u32::try_from(line).ok())
                            .ok_or(AppError::Internal)?,
                    ),
                },
            })
        })
        .collect()
}

pub(super) fn from_json<T: serde::de::DeserializeOwned>(source: &str) -> Result<T, StoreError> {
    serde_json::from_str(source).map_err(|_| AppError::Internal.into())
}

pub(super) fn optional<T: std::str::FromStr>(
    row: &Row<'_>,
    column: usize,
) -> Result<Option<T>, StoreError> {
    let value: Option<String> = row.get(column)?;
    value.as_deref().map(parse).transpose()
}

pub(super) fn finding_id(number: i64) -> Result<FindingId, StoreError> {
    u32::try_from(number)
        .ok()
        .and_then(|number| FindingId::new(number).ok())
        .ok_or_else(|| AppError::Internal.into())
}

pub(super) fn revision(value: i64) -> Result<RecordRevision, StoreError> {
    RecordRevision::try_from(value).map_err(|_| AppError::Internal.into())
}

/// The fields that differ between two revisions of one finding.
pub(super) fn finding_changes(before: &FindingRecord, after: &FindingRecord) -> Vec<RecordField> {
    let (old, new) = (&before.fields, &after.fields);
    let (old_body, new_body) = (&old.body, &new.body);
    let old_outcome = before.outcome.as_ref();
    let new_outcome = after.outcome.as_ref();
    let pairs = [
        (RecordField::Title, old.title != new.title),
        (RecordField::Severity, old.severity != new.severity),
        (RecordField::Blocking, old.blocking != new.blocking),
        (RecordField::Kind, old.kind != new.kind),
        (RecordField::Category, old.category != new.category),
        (
            RecordField::Locations,
            old_body.locations != new_body.locations,
        ),
        (RecordField::Issue, old_body.issue != new_body.issue),
        (
            RecordField::Consequence,
            old_body.consequence != new_body.consequence,
        ),
        (
            RecordField::Evidence,
            old_body.evidence != new_body.evidence,
        ),
        (
            RecordField::ProposedComment,
            old_body.proposed_comment != new_body.proposed_comment,
        ),
        (
            RecordField::ReplyToNote,
            old_body.reply_to_note != new_body.reply_to_note,
        ),
        (RecordField::Details, old_body.details != new_body.details),
        (
            RecordField::Outcome,
            old_outcome.map(|o| o.conclusion) != new_outcome.map(|o| o.conclusion),
        ),
        (
            RecordField::Reason,
            old_outcome.map(|o| &o.reason) != new_outcome.map(|o| &o.reason),
        ),
        (
            RecordField::DiscussionIds,
            old_outcome.map(|o| &o.discussion_ids) != new_outcome.map(|o| &o.discussion_ids),
        ),
        (
            RecordField::CheckedSha,
            old_outcome.map(|o| &o.checked_sha) != new_outcome.map(|o| &o.checked_sha),
        ),
    ];
    pairs
        .into_iter()
        .filter_map(|(field, differs)| differs.then_some(field))
        .collect()
}
