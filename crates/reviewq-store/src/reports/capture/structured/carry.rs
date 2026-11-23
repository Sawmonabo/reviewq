use std::collections::{BTreeMap, BTreeSet};

use reviewq_app::access::AppError;
use reviewq_app::reports::{Descriptor, ReportEdits};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::StoreError;

// Offsets belong to the indicated accepted receipt. The same-run indexes
// supply their exact title, span and hash when a page displays them.
#[derive(Default, Serialize)]
pub(super) struct Carry {
    findings: BTreeMap<String, String>,
    sections: BTreeMap<String, Vec<i64>>,
}

impl Carry {
    pub(super) fn is_empty(&self) -> bool {
        self.findings.is_empty() && self.sections.is_empty()
    }
}

pub(super) fn prepare(
    db: &Connection,
    run: &str,
    previous: Option<&str>,
    edits: &ReportEdits,
    fresh_sections: &[Descriptor],
) -> Result<Carry, StoreError> {
    let Some(previous) = previous else {
        return Ok(Carry::default());
    };
    let changed: BTreeSet<&str> = edits
        .findings
        .iter()
        .map(|edit| edit.finding_id.as_str())
        .collect();
    let mut carry = Carry::default();
    let mut replaced_ranges = BTreeMap::<String, Vec<(i64, i64)>>::new();
    let mut query = db.prepare_cached(
        "SELECT f.finding_id,coalesce(inherited.value,f.receipt_id),
                coalesce(prior.scalar_offset,f.scalar_offset),coalesce(prior.scalar_length,f.scalar_length)
         FROM report_findings f LEFT JOIN report_handoffs h USING(receipt_id)
         LEFT JOIN json_each(h.edits,'$.carry.findings') inherited ON inherited.key=f.finding_id
         LEFT JOIN report_findings prior ON prior.run_id=f.run_id AND prior.receipt_id=inherited.value
                                       AND prior.finding_id=f.finding_id
         WHERE f.run_id=?1 AND f.receipt_id=?2 AND coalesce(prior.scalar_offset,f.scalar_offset) IS NOT NULL",
    )?;
    let mut rows = query.query(params![run, previous])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let origin: String = row.get(1)?;
        if changed.contains(id.as_str()) {
            let offset: i64 = row.get(2)?;
            let length: i64 = row.get(3)?;
            let end = offset.checked_add(length).ok_or(AppError::Internal)?;
            replaced_ranges
                .entry(origin)
                .or_default()
                .push((offset, end));
        } else {
            carry.findings.insert(id, origin);
        }
    }
    drop(rows);

    let retained_fresh: BTreeSet<(&str, &str)> = fresh_sections
        .iter()
        .filter_map(|entry| match entry {
            Descriptor::Section { title, source, .. } => {
                Some((title.as_str(), source.hash.as_str()))
            }
            _ => None,
        })
        .collect();
    let mut query = db.prepare_cached(SECTIONS)?;
    let mut rows = query.query(params![run, previous])?;
    while let Some(row) = rows.next()? {
        let origin: String = row.get(0)?;
        let title: String = row.get(1)?;
        let offset: i64 = row.get(2)?;
        let length: i64 = row.get(3)?;
        let hash: String = row.get(4)?;
        let end = offset.checked_add(length).ok_or(AppError::Internal)?;
        if retained_fresh.contains(&(title.as_str(), hash.as_str()))
            || edits
                .sections
                .iter()
                .any(|edit| edit.baseline_title == title && edit.baseline_hash.as_str() == hash)
            || replaced_ranges.get(&origin).is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|&(start, stop)| start <= offset && end <= stop)
            })
        {
            continue;
        }
        carry.sections.entry(origin).or_default().push(offset);
    }
    Ok(carry)
}

pub(super) fn matches_section(
    db: &Connection,
    run: &str,
    receipt: &str,
    title: &str,
    hash: &str,
) -> Result<bool, StoreError> {
    let sql = format!("SELECT count(*)=1 FROM ({SECTIONS}) WHERE title=?3 AND evidence_sha256=?4");
    Ok(db.query_row(&sql, params![run, receipt, title, hash], |row| row.get(0))?)
}

const SECTIONS: &str = "
    SELECT receipt_id,title,scalar_offset,scalar_length,evidence_sha256
    FROM report_sections WHERE run_id=?1 AND receipt_id=?2
    UNION ALL
    SELECT s.receipt_id,s.title,s.scalar_offset,s.scalar_length,s.evidence_sha256
    FROM report_handoffs h JOIN json_each(h.edits,'$.carry.sections') origins
    JOIN json_each(origins.value) offsets
    JOIN report_sections s ON s.run_id=h.run_id AND s.receipt_id=origins.key AND s.scalar_offset=offsets.value
    WHERE h.run_id=?1 AND h.receipt_id=?2
    ORDER BY receipt_id,scalar_offset";
