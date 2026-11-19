use reviewq_app::access::AppError;
use reviewq_app::reports::{
    Collection, ContextKey, ContextPosition, ContextRequest, Descriptor, ReportContext, SourceRange,
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
            "SELECT finding_id, title, severity, blocking, scalar_offset, scalar_length, evidence_sha256
             FROM report_findings WHERE run_id=?1 AND receipt_id=?2 AND finding_id>?3
             ORDER BY finding_id LIMIT ?5",
        Collection::Changes =>
            "SELECT finding_id, conclusion, reason, checked_sha
             FROM report_changes WHERE run_id=?1 AND receipt_id=?2 AND finding_id>?3
             ORDER BY finding_id LIMIT ?5",
        Collection::Sections =>
            "SELECT title, scalar_offset, scalar_length, evidence_sha256
             FROM report_sections WHERE run_id=?1 AND receipt_id=?2 AND scalar_offset>?3
             ORDER BY scalar_offset LIMIT ?5",
        Collection::Dispositions =>
            "SELECT f.finding_id, d.blocking, d.publication, d.reason
             FROM report_findings f JOIN finding_dispositions d
               ON d.run_id=f.run_id AND d.finding_id=f.finding_id AND d.disposition_version=(
                 SELECT MAX(h.disposition_version) FROM finding_dispositions h
                 WHERE h.run_id=f.run_id AND h.finding_id=f.finding_id AND h.disposition_version<=?4
               )
             WHERE f.run_id=?1 AND f.receipt_id=?2 AND f.finding_id>?3 AND d.removed=0
             ORDER BY f.finding_id LIMIT ?5",
    }
}

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
            severity: row.get(2)?,
            blocking: row.get(3)?,
            source: (row.get::<_, Option<i64>>(4)?.is_some())
                .then(|| range(4))
                .transpose()?,
        },
        Collection::Changes => Descriptor::Change {
            id: row.get(0)?,
            conclusion: parse(text(row, 1)?)?,
            reason: row.get(2)?,
            checked_sha: parse(text(row, 3)?)?,
        },
        Collection::Sections => Descriptor::Section {
            title: row.get(0)?,
            source: range(1)?,
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
