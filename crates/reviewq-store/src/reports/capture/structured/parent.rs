use std::collections::BTreeSet;

use reviewq_app::access::AppError;
use reviewq_app::reports::FindingConclusion;
use reviewq_app::reports::ledger::{Parent, ParentFinding};
use rusqlite::{Connection, params};

use crate::StoreError;
use crate::queue::parse;
use crate::reports::optional;

/// Reads the parent a final builds on: the active findings of the accepted version and the
/// IDs this run's retiring outcomes retired that it does not hold active.
///
/// The active findings are the version's rows minus the IDs its own outcomes retire. A
/// version accepted before the findings ledger kept rows for retired cards whose headings
/// stayed in its text. Each finding's origin is the receipt whose card it carries, else
/// this version. Both callers pass the run's head, and acceptance makes each new version
/// the head with the previous head as its parent, so the run's outcomes are exactly those
/// of the parent and its ancestors.
pub(in crate::reports::capture) fn read(
    db: &Connection,
    run: &str,
    receipt: &str,
) -> Result<Parent, StoreError> {
    let findings = active(db, run, receipt)?;
    let mut query =
        db.prepare_cached("SELECT finding_id, conclusion FROM report_changes WHERE run_id=?1")?;
    let mut rows = query.query([run])?;
    let mut retired = BTreeSet::new();
    while let Some(row) = rows.next()? {
        let conclusion: Option<FindingConclusion> = optional(row, 1)?;
        if conclusion.is_some_and(FindingConclusion::retires_finding) {
            retired.insert(row.get::<_, String>(0)?);
        }
    }
    for finding in &findings {
        retired.remove(&finding.id);
    }
    Ok(Parent { findings, retired })
}

fn active(db: &Connection, run: &str, receipt: &str) -> Result<Vec<ParentFinding>, StoreError> {
    let mut query = db.prepare_cached(
        "SELECT f.finding_id, f.title, f.severity, f.blocking, f.evidence_sha256, details.value,
                coalesce(inherited.value, f.receipt_id), c.conclusion
         FROM report_findings f LEFT JOIN report_handoffs h ON h.receipt_id=f.receipt_id
         LEFT JOIN json_each(h.edits,'$.acceptedDetails') details ON details.key=f.finding_id
         LEFT JOIN json_each(h.edits,'$.carry.findings') inherited ON inherited.key=f.finding_id
         LEFT JOIN report_changes c ON c.run_id=f.run_id AND c.receipt_id=f.receipt_id
                                   AND c.finding_id=f.finding_id
         WHERE f.run_id=?1 AND f.receipt_id=?2 ORDER BY f.finding_id",
    )?;
    let mut rows = query.query(params![run, receipt])?;
    let mut findings = Vec::new();
    while let Some(row) = rows.next()? {
        let conclusion: Option<FindingConclusion> = optional(row, 7)?;
        if conclusion.is_some_and(FindingConclusion::retires_finding) {
            continue;
        }
        let details: Option<String> = row.get(5)?;
        findings.push(ParentFinding {
            id: row.get(0)?,
            title: row.get(1)?,
            severity: row.get(2)?,
            blocking: row.get(3)?,
            evidence_sha256: optional(row, 4)?,
            details: details
                .map(|value| serde_json::from_str(&value).map_err(|_| AppError::Internal))
                .transpose()?,
            origin: parse(&row.get::<_, String>(6)?)?,
        });
    }
    Ok(findings)
}
