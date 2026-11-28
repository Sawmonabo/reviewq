use reviewq_app::access::AppError;
use reviewq_app::reports::{
    Collection, ContextKey, ContextPosition, ContextRequest, Descriptor, FindingBasis,
    FindingChange, FindingDisposition, FindingPreview, FindingState, ReportContext, SourceRange,
};
use reviewq_domain::identity::{Revision, RunId};
use rusqlite::{Connection, Row, params};

use super::{current_receipt, optional, read_count, version};
use crate::StoreError;
use crate::ledger::locations;
use crate::queue::{parse, text};

pub(super) fn read(
    connection: &Connection,
    run: &RunId,
    request: ContextRequest,
) -> Result<ReportContext, StoreError> {
    let ContextRequest {
        receipt,
        collection,
        findings,
        after,
        limit,
    } = request;
    let selected = match (&receipt, &after) {
        (Some(receipt), Some(page)) if receipt != &page.report.receipt_id => {
            return Err(AppError::CursorExpired.into());
        }
        (Some(receipt), _) => Some(receipt.clone()),
        (None, Some(page)) => Some(page.report.receipt_id.clone()),
        (None, None) => current_receipt(connection, run)?,
    };
    let Some(receipt) = selected else {
        return Ok(ReportContext {
            report: None,
            disposition_version: revision(0)?,
            items: Vec::new(),
            next: None,
        });
    };
    let report = version(connection, run, &receipt)?.summary.reference;
    let revision_value = connection.query_row(
        "SELECT COALESCE(v.closing_disposition_version, h.disposition_version,
                         v.starting_disposition_version)
         FROM report_versions v LEFT JOIN report_heads h
           ON h.run_id=v.run_id AND h.receipt_id=v.receipt_id
         WHERE v.run_id=?1 AND v.receipt_id=?2",
        params![run.as_str(), receipt.as_str()],
        |row| row.get(0),
    )?;
    let disposition_version = revision(revision_value)?;
    if after.as_ref().is_some_and(|after| {
        after.report != report
            || (collection == Collection::Dispositions
                && after.disposition_version != disposition_version)
    }) {
        return Err(AppError::CursorExpired.into());
    }

    let key = after.map(|page| page.key);
    let mut items = Vec::with_capacity(usize::from(limit));
    let mut statement = connection.prepare_cached(query(collection))?;
    let key_value: rusqlite::types::Value = match (&key, collection) {
        (Some(ContextKey::Offset(offset)), Collection::Sections) => i64::try_from(*offset)
            .map_err(|_| AppError::CursorExpired)?
            .into(),
        (Some(ContextKey::Identity(id)), collection) if collection != Collection::Sections => {
            id.clone().into()
        }
        (None, Collection::Sections) => (-1_i64).into(),
        (None, _) => String::new().into(),
        _ => return Err(AppError::CursorExpired.into()),
    };
    let shared = params![
        run.as_str(),
        receipt.as_str(),
        key_value,
        disposition_version.as_sqlite_integer(),
        limit + 1,
    ];
    let mut rows = if collection == Collection::Findings {
        let kind = findings.kind.map(|kind| kind.to_string());
        let open = findings.state != FindingState::Retired;
        let retired = findings.state != FindingState::Open;
        let mut values = shared.to_vec();
        values.extend_from_slice(params![open, retired, kind, findings.full_text]);
        statement.query(values.as_slice())?
    } else {
        statement.query(shared)?
    };
    let mut next = None;
    while let Some(row) = rows.next()? {
        if items.len() == usize::from(limit) {
            next = items.last().map(|item: &Descriptor| ContextPosition {
                report: report.clone(),
                disposition_version,
                key: item.key(),
            });
            break;
        }
        items.push(decode(collection, row)?);
    }
    Ok(ReportContext {
        report: Some(report),
        disposition_version,
        items,
        next,
    })
}

/// The page query of each collection. Finding IDs are canonical (`F-01`, `F-99`, `F-100`), so
/// ordering and paging by length, then text, follow the finding number.
///
/// A `Findings` read selects from what the version itself records: the findings it shows
/// (`report_findings`, when `?6`), and the findings it retires (when `?7`), which are its
/// outcomes (`report_changes`) on findings it no longer shows, each with the row its parent
/// version showed. `?8` names a kind, `?9` asks for the issue and proposed comment. A finding's
/// fields come from `ledger_changes` at the record revision the version published, so a later
/// unsubmitted revision is never read.
fn query(collection: Collection) -> &'static str {
    match collection {
        Collection::Findings => {
            "SELECT x.finding_id, x.title, x.severity, x.blocking, x.record_revision,
                    x.scalar_offset, x.scalar_length, x.evidence_sha256,
                    d.blocking, d.publication, d.reason,
                    c.kind, c.locations, c.consequence, c.evidence_kind, c.evidence,
                    CASE WHEN ?9 THEN c.issue END, CASE WHEN ?9 THEN c.proposed_comment END,
                    CASE WHEN ?9 THEN c.reply_to_note END,
                    x.retired, o.conclusion, o.reason, o.checked_sha
             FROM (
                 SELECT f.finding_id, f.title, f.severity, f.blocking, f.record_revision,
                        f.scalar_offset, f.scalar_length, f.evidence_sha256, 0 AS retired
                 FROM report_findings f
                 WHERE ?6 AND f.run_id=?1 AND f.receipt_id=?2
                   AND (length(f.finding_id), f.finding_id)>(length(?3), ?3)
                 UNION ALL
                 SELECT p.finding_id, p.title, p.severity, p.blocking, p.record_revision,
                        NULL, NULL, NULL, 1
                 FROM report_changes g
                 JOIN report_versions v ON v.run_id=g.run_id AND v.receipt_id=g.receipt_id
                 JOIN report_findings p ON p.run_id=g.run_id AND p.receipt_id=v.parent_receipt_id
                   AND p.finding_id=g.finding_id
                 WHERE ?7 AND g.run_id=?1 AND g.receipt_id=?2
                   AND (length(g.finding_id), g.finding_id)>(length(?3), ?3)
                   AND NOT EXISTS (SELECT 1 FROM report_findings s WHERE s.run_id=g.run_id
                     AND s.receipt_id=g.receipt_id AND s.finding_id=g.finding_id)
             ) x
             LEFT JOIN ledger_changes c ON c.run_id=?1 AND c.finding IS NOT NULL
               AND c.finding=CAST(substr(x.finding_id, 3) AS INTEGER)
               AND c.revision=x.record_revision
             LEFT JOIN report_changes o ON o.run_id=?1 AND o.receipt_id=?2
               AND o.finding_id=x.finding_id
             LEFT JOIN finding_dispositions d ON d.run_id=?1 AND d.finding_id=x.finding_id
               AND d.removed=0 AND d.disposition_version=(
                 SELECT MAX(a.disposition_version) FROM finding_dispositions a
                 WHERE a.run_id=?1 AND a.finding_id=x.finding_id AND a.disposition_version<=?4
               )
             WHERE ?8 IS NULL OR c.kind=?8
             ORDER BY length(x.finding_id), x.finding_id LIMIT ?5"
        }
        Collection::Changes => {
            "SELECT c.finding_id, c.conclusion, c.reason, c.checked_sha,
                (SELECT json_group_array(d.discussion_id) FROM (
                    SELECT discussion_id FROM report_change_discussions
                    WHERE run_id=c.run_id AND receipt_id=c.receipt_id AND finding_id=c.finding_id
                    ORDER BY discussion_id
                ) d)
             FROM report_changes c WHERE c.run_id=?1 AND c.receipt_id=?2
               AND (length(c.finding_id), c.finding_id)>(length(?3), ?3)
             ORDER BY length(c.finding_id), c.finding_id LIMIT ?5"
        }
        Collection::Sections => {
            "SELECT title, scalar_offset, scalar_length, evidence_sha256
             FROM report_sections WHERE run_id=?1 AND receipt_id=?2 AND scalar_offset>?3
             ORDER BY scalar_offset LIMIT ?5"
        }
        Collection::Dispositions => {
            "WITH available(finding_id) AS (
                 SELECT finding_id FROM report_findings WHERE run_id=?1 AND receipt_id=?2
                   AND (length(finding_id), finding_id)>(length(?3), ?3)
                 UNION
                 SELECT finding_id FROM finding_dispositions
                 WHERE run_id=?1 AND receipt_id=?2 AND disposition_version<=?4
                   AND (length(finding_id), finding_id)>(length(?3), ?3)
             )
             SELECT a.finding_id, d.blocking, d.publication, d.reason
             FROM available a JOIN finding_dispositions d ON d.run_id=?1
               AND d.finding_id=a.finding_id AND d.disposition_version=(
                 SELECT MAX(h.disposition_version) FROM finding_dispositions h
                 WHERE h.run_id=?1 AND h.finding_id=a.finding_id AND h.disposition_version<=?4
               )
             WHERE d.removed=0 ORDER BY length(a.finding_id), a.finding_id LIMIT ?5"
        }
    }
}

/// Decodes one row of `query(collection)`.
fn decode(collection: Collection, row: &Row<'_>) -> Result<Descriptor, StoreError> {
    let range = |column: usize| -> Result<SourceRange, StoreError> {
        Ok(SourceRange {
            offset: read_count(row, column)?,
            length: read_count(row, column + 1)?,
            hash: parse(text(row, column + 2)?)?,
        })
    };
    Ok(match collection {
        Collection::Findings => Descriptor::Finding {
            id: row.get(0)?,
            title: row.get(1)?,
            severity: parse(text(row, 2)?)?,
            blocking: row.get(3)?,
            source: match row.get_ref(5)? {
                rusqlite::types::ValueRef::Null => None,
                _ => Some(range(5)?),
            },
            details: finding_fields(row, 11)?.map(Box::new),
            retired: row.get(19)?,
            outcome: optional(row, 20)?
                .map(|conclusion| {
                    Ok::<_, StoreError>(Box::new(FindingChange {
                        conclusion,
                        reason: row.get(21)?,
                        checked_sha: optional(row, 22)?,
                    }))
                })
                .transpose()?,
            disposition: row
                .get::<_, Option<String>>(10)?
                .map(|reason| {
                    Ok::<_, StoreError>(Box::new(FindingDisposition {
                        blocking: row.get(8)?,
                        publication: optional(row, 9)?,
                        reason,
                    }))
                })
                .transpose()?,
        },
        Collection::Changes => Descriptor::Change {
            id: row.get(0)?,
            conclusion: parse(text(row, 1)?)?,
            reason: row.get(2)?,
            checked_sha: optional(row, 3)?,
            discussion_ids: serde_json::from_str(text(row, 4)?).map_err(|_| AppError::Internal)?,
        },
        Collection::Sections => Descriptor::Section {
            title: row.get(0)?,
            source: range(1)?,
            cursor_offset: read_count(row, 1)?,
        },
        Collection::Dispositions => Descriptor::Disposition {
            id: row.get(0)?,
            blocking: row.get(1)?,
            publication: optional(row, 2)?,
            reason: row.get(3)?,
        },
    })
}

/// The published revision's fields from column `at`, or `None` when its `ledger_changes` row
/// is missing.
fn finding_fields(row: &Row<'_>, at: usize) -> Result<Option<FindingPreview>, StoreError> {
    let Some(kind) = optional(row, at)? else {
        return Ok(None);
    };
    Ok(Some(FindingPreview {
        kind,
        locations: locations(text(row, at + 1)?)?,
        consequence: row.get(at + 2)?,
        evidence: FindingBasis {
            kind: parse(text(row, at + 3)?)?,
            text: row.get(at + 4)?,
        },
        issue: row.get(at + 5)?,
        proposed_comment: row.get(at + 6)?,
        reply_to_note: row.get(at + 7)?,
    }))
}

fn revision(value: i64) -> Result<Revision, StoreError> {
    Revision::try_from(value).map_err(|_| AppError::Internal.into())
}
