use std::collections::{BTreeMap, BTreeSet};

use reviewq_app::access::AppError;
use reviewq_app::reports::{
    FindingDetails, ReportEdits, ReportScope, RevisionEvidence, StructuredFinding, StructuredReport,
};
use reviewq_domain::identity::ReportHash;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::StoreError;

mod carry;

pub(super) struct Projection {
    pub count: i64,
    retain: bool,
    replaced: Vec<String>,
    metadata: String,
}

#[derive(Serialize)]
struct AcceptedMetadata {
    #[serde(rename = "acceptedDetails")]
    details: BTreeMap<String, FindingDetails>,
    #[serde(skip_serializing_if = "carry::Carry::is_empty")]
    carry: carry::Carry,
}

/// Installs the submitted cards over the parent's active findings. A scoped
/// result also carries the parent cards it neither rewrites nor retires.
pub(super) fn prepare(
    db: &Connection,
    run: &str,
    previous: Option<&str>,
    report: &StructuredReport,
    revision: &RevisionEvidence,
    edits: &ReportEdits,
) -> Result<Projection, StoreError> {
    let retain = report.scope != ReportScope::FullReview;
    let previous = previous.filter(|_| retain);
    let mut prior = BTreeSet::new();
    let mut details: BTreeMap<String, FindingDetails> = BTreeMap::new();
    if let Some(receipt) = previous {
        let mut statement = db.prepare_cached(
            "SELECT finding_id FROM report_findings
             WHERE run_id=?1 AND receipt_id=?2 ORDER BY finding_id",
        )?;
        let rows = statement.query_map(params![run, receipt], |row| row.get::<_, String>(0))?;
        prior = rows.collect::<Result<_, _>>()?;
        let encoded: Option<String> = db.query_row(
            "SELECT json_extract(edits, '$.acceptedDetails') FROM report_handoffs WHERE receipt_id=?1",
            [receipt],
            |row| row.get(0),
        ).optional()?.flatten();
        if let Some(encoded) = encoded {
            details = serde_json::from_str(&encoded).map_err(|_| AppError::Internal)?;
        }
    }
    let mut replaced = Vec::new();
    for edit in &edits.findings {
        if edit.conclusion.retires_finding() {
            prior.remove(&edit.finding_id);
            details.remove(&edit.finding_id);
            replaced.push(edit.finding_id.clone());
        }
    }
    let checked_sha = match revision {
        RevisionEvidence::Exact(sha) => Some(sha.as_str()),
        RevisionEvidence::Unknown(_) => None,
    };
    let mut count = prior.len();
    for finding in &report.findings {
        count += usize::from(!prior.contains(&finding.id));
        let mut current = finding.details.clone();
        current.checked_sha = checked_sha.map(str::to_owned);
        details.insert(finding.id.clone(), current);
        replaced.push(finding.id.clone());
    }
    let carry = carry::prepare(db, run, previous, &replaced)?;
    let metadata = serde_json::to_string(&AcceptedMetadata { details, carry })
        .map_err(|_| AppError::Internal)?;
    Ok(Projection {
        count: count as i64,
        retain,
        replaced,
        metadata,
    })
}

pub(super) fn full_projection_fits(
    db: &Connection,
    pending: &str,
    reviewed_sha: Option<&str>,
) -> Result<bool, StoreError> {
    Ok(db.query_row(
        "SELECT CASE WHEN json_extract(?1,'$.structured.scope')='full_review' THEN
          length(CAST(json_set(json_remove(?1,'$.structured.findings'),'$.acceptedDetails',(
            SELECT json_group_object(json_extract(value,'$.id'), json(CASE WHEN ?2 IS NULL
              THEN json_extract(value,'$.details')
              ELSE json_set(json_extract(value,'$.details'),'$.checkedSha',?2) END))
            FROM json_each(?1,'$.structured.findings')
          )) AS BLOB))<=131072 ELSE 1 END",
        params![pending, reviewed_sha],
        |row| row.get(0),
    )?)
}

impl Projection {
    pub(super) fn fits(&self, db: &Connection, receipt: &str) -> Result<bool, StoreError> {
        Ok(db.query_row(
            "SELECT length(CAST(json_patch(json_remove(edits,'$.structured.findings'),?2)
                  AS BLOB))<=131072 FROM report_handoffs WHERE receipt_id=?1",
            params![receipt, self.metadata],
            |row| row.get(0),
        )?)
    }

    pub(super) fn install(
        &self,
        db: &Connection,
        run: &str,
        receipt: &str,
        previous: Option<&str>,
        supplied: &[StructuredFinding],
    ) -> Result<(), StoreError> {
        if let Some(parent) = previous.filter(|_| self.retain) {
            let replaced = serde_json::to_string(&self.replaced).map_err(|_| AppError::Internal)?;
            db.execute(
                "INSERT INTO report_findings
                 SELECT run_id,?3,finding_id,title,severity,blocking,NULL,NULL,evidence_sha256
                 FROM report_findings WHERE run_id=?1 AND receipt_id=?2
                   AND finding_id NOT IN (SELECT value FROM json_each(?4))",
                params![run, parent, receipt, replaced],
            )?;
        }
        let mut insert = db.prepare_cached(
            "INSERT INTO report_findings(run_id,receipt_id,finding_id,title,severity,blocking,evidence_sha256)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
        )?;
        for finding in supplied {
            let authored = serde_json::to_vec(finding).map_err(|_| AppError::Internal)?;
            let digest = ReportHash::from_sha256(Sha256::digest(authored).into());
            insert.execute(params![
                run,
                receipt,
                finding.id,
                finding.title,
                finding.severity,
                finding.blocking,
                digest.as_str(),
            ])?;
        }
        db.execute(
            "UPDATE report_handoffs SET edits=json_patch(json_remove(edits,'$.structured.findings'),?2)
                  WHERE receipt_id=?1",
            params![receipt, self.metadata],
        )?;
        Ok(())
    }
}
