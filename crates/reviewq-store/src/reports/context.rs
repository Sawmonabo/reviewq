use reviewq_app::access::AppError;
use reviewq_app::reports::{
    Collection, ContextKey, ContextPosition, ContextRequest, Descriptor, FindingDisposition,
    ReportContext, SourceRange,
};
use reviewq_domain::identity::{Revision, RunId};
use rusqlite::{Connection, Row, params};

use super::{current_receipt, optional, read_count, version};
use crate::StoreError;
use crate::queue::{parse, text};

pub(super) fn read(
    connection: &Connection,
    run: &RunId,
    request: ContextRequest,
) -> Result<ReportContext, StoreError> {
    let ContextRequest {
        receipt,
        collection,
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
    let mut rows = statement.query(params![
        run.as_str(),
        receipt.as_str(),
        key_value,
        disposition_version.as_sqlite_integer(),
        limit + 1,
    ])?;
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

fn query(collection: Collection) -> &'static str {
    match collection {
        Collection::Findings =>
            "SELECT f.finding_id, f.title, f.severity, f.blocking, f.section, f.category,
                    f.record_revision, f.scalar_offset, f.scalar_length, f.evidence_sha256,
                    d.blocking, d.publication, d.reason
             FROM report_findings f
             LEFT JOIN finding_dispositions d ON d.run_id=f.run_id AND d.finding_id=f.finding_id
               AND d.removed=0 AND d.disposition_version=(
                 SELECT MAX(a.disposition_version) FROM finding_dispositions a
                 WHERE a.run_id=f.run_id AND a.finding_id=f.finding_id AND a.disposition_version<=?4
               )
             WHERE f.run_id=?1 AND f.receipt_id=?2 AND f.finding_id>?3
             ORDER BY f.finding_id LIMIT ?5",
        Collection::Changes =>
            "SELECT c.finding_id, c.conclusion, c.reason, c.checked_sha,
                (SELECT json_group_array(d.discussion_id) FROM (
                    SELECT discussion_id FROM report_change_discussions
                    WHERE run_id=c.run_id AND receipt_id=c.receipt_id AND finding_id=c.finding_id
                    ORDER BY discussion_id
                ) d)
             FROM report_changes c WHERE c.run_id=?1 AND c.receipt_id=?2 AND c.finding_id>?3
             ORDER BY c.finding_id LIMIT ?5",
        Collection::Sections =>
            "SELECT title, scalar_offset, scalar_length, evidence_sha256
             FROM report_sections WHERE run_id=?1 AND receipt_id=?2 AND scalar_offset>?3
             ORDER BY scalar_offset LIMIT ?5",
        Collection::Dispositions =>
            "WITH available(finding_id) AS (
                 SELECT finding_id FROM report_findings WHERE run_id=?1 AND receipt_id=?2 AND finding_id>?3
                 UNION
                 SELECT finding_id FROM finding_dispositions
                 WHERE run_id=?1 AND receipt_id=?2 AND finding_id>?3 AND disposition_version<=?4
             )
             SELECT a.finding_id, d.blocking, d.publication, d.reason
             FROM available a JOIN finding_dispositions d ON d.run_id=?1
               AND d.finding_id=a.finding_id AND d.disposition_version=(
                 SELECT MAX(h.disposition_version) FROM finding_dispositions h
                 WHERE h.run_id=?1 AND h.finding_id=a.finding_id AND h.disposition_version<=?4
               )
             WHERE d.removed=0 ORDER BY a.finding_id LIMIT ?5",
    }
}

/// Decodes one row of `query(collection)`. A finding's details stay `None` until K2 reads
/// them from the record revision in `ledger_changes`.
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
            section: parse(text(row, 4)?)?,
            category: row.get(5)?,
            record_revision: revision(row.get(6)?)?,
            source: range(7)?,
            details: None,
            disposition: row
                .get::<_, Option<String>>(12)?
                .map(|reason| {
                    Ok::<_, StoreError>(Box::new(FindingDisposition {
                        blocking: row.get(10)?,
                        publication: optional(row, 11)?,
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

fn revision(value: i64) -> Result<Revision, StoreError> {
    Revision::try_from(value).map_err(|_| AppError::Internal.into())
}
