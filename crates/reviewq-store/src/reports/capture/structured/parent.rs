use reviewq_app::access::AppError;
use reviewq_app::reports::FindingConclusion;
use reviewq_app::reports::ledger::ParentFinding;
use rusqlite::{Connection, params};

use crate::StoreError;
use crate::queue::parse;
use crate::reports::optional;

/// Reads the active findings of an accepted version: its rows minus the IDs its own
/// outcomes retire. A version accepted before the findings ledger kept rows for retired
/// cards whose headings stayed in its text. Each finding's origin is the receipt whose
/// card it carries, else this version.
pub(in crate::reports::capture) fn active(
    db: &Connection,
    run: &str,
    receipt: &str,
) -> Result<Vec<ParentFinding>, StoreError> {
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
