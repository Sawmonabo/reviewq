use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::StoreError;

// Offsets belong to the indicated accepted receipt. The same-run indexes
// supply their exact title, span and hash when a page displays them.
#[derive(Default, Serialize)]
pub(super) struct Carry {
    findings: BTreeMap<String, String>,
}

impl Carry {
    pub(super) fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Links each untouched parent card to the receipt that last authored its span.
pub(super) fn prepare(
    db: &Connection,
    run: &str,
    previous: Option<&str>,
    replaced: &[String],
) -> Result<Carry, StoreError> {
    let Some(previous) = previous else {
        return Ok(Carry::default());
    };
    let replaced: BTreeSet<&str> = replaced.iter().map(String::as_str).collect();
    let mut carry = Carry::default();
    let mut query = db.prepare_cached(
        "SELECT f.finding_id,coalesce(inherited.value,f.receipt_id)
         FROM report_findings f LEFT JOIN report_handoffs h USING(receipt_id)
         LEFT JOIN json_each(h.edits,'$.carry.findings') inherited ON inherited.key=f.finding_id
         LEFT JOIN report_findings prior ON prior.run_id=f.run_id AND prior.receipt_id=inherited.value
                                       AND prior.finding_id=f.finding_id
         WHERE f.run_id=?1 AND f.receipt_id=?2 AND coalesce(prior.scalar_offset,f.scalar_offset) IS NOT NULL",
    )?;
    let mut rows = query.query(params![run, previous])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if !replaced.contains(id.as_str()) {
            carry.findings.insert(id, row.get(1)?);
        }
    }
    Ok(carry)
}
