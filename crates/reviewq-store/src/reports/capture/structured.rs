use std::collections::BTreeMap;

use reviewq_app::access::AppError;
use reviewq_app::reports::ledger::{Anchor, Ledger};
use reviewq_app::reports::{Descriptor, FindingDetails, METADATA_BYTES, StructuredReport};
use reviewq_domain::identity::ReportHash;
use rusqlite::{Connection, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{db_int, edits};
use crate::StoreError;

pub(super) mod parent;

const METADATA_LIMIT: i64 = METADATA_BYTES as i64;

/// A derived version and the metadata its acceptance adds to the handoff.
pub(super) struct Projection {
    ledger: Ledger,
    metadata: String,
}

// Carry offsets belong to the indicated accepted receipt. Its rows supply the exact
// span and hash when a page displays them.
#[derive(Serialize)]
struct AcceptedMetadata<'a> {
    #[serde(rename = "acceptedDetails")]
    details: BTreeMap<&'a str, &'a FindingDetails>,
    #[serde(skip_serializing_if = "Carry::is_empty")]
    carry: Carry<'a>,
}

#[derive(Default, Serialize)]
struct Carry<'a> {
    findings: BTreeMap<&'a str, &'a str>,
}

impl Carry<'_> {
    fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }
}

impl Projection {
    /// Serializes the derived details, including those carried from the parent, and the
    /// origin receipt of each finding whose anchor is carried.
    pub(super) fn new(ledger: Ledger) -> Result<Self, StoreError> {
        let mut details = BTreeMap::new();
        let mut carry = Carry::default();
        for finding in &ledger.findings {
            if let Some(value) = &finding.details {
                details.insert(finding.id.as_str(), value);
            }
            if let Anchor::Carried(origin) = &finding.anchor {
                carry.findings.insert(finding.id.as_str(), origin.as_str());
            }
        }
        let metadata = serde_json::to_string(&AcceptedMetadata { details, carry })
            .map_err(|_| AppError::Internal)?;
        Ok(Self { ledger, metadata })
    }

    pub(super) fn metadata(&self) -> &str {
        &self.metadata
    }

    pub(super) fn finding_count(&self) -> i64 {
        self.ledger.findings.len() as i64
    }

    pub(super) fn change_count(&self) -> i64 {
        self.ledger.changes.len() as i64
    }

    /// Writes the derived rows, this report's sections and outcomes, and the accepted
    /// metadata. A carried finding keeps no span of its own.
    pub(super) fn install(
        &self,
        db: &Connection,
        run: &str,
        receipt: &str,
        report: &StructuredReport,
        index: Option<&[Descriptor]>,
    ) -> Result<(), StoreError> {
        let mut insert = db.prepare_cached(
            "INSERT INTO report_findings(run_id,receipt_id,finding_id,title,severity,blocking,
               scalar_offset,scalar_length,evidence_sha256) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?;
        for finding in &self.ledger.findings {
            let span = match &finding.anchor {
                Anchor::Heading(source) => Some((db_int(source.offset)?, db_int(source.length)?)),
                Anchor::Carried(_) | Anchor::None => None,
            };
            let evidence = match &finding.evidence_sha256 {
                Some(hash) => Some(hash.clone()),
                None => authored_digest(report, &finding.id)?,
            };
            insert.execute(params![
                run,
                receipt,
                finding.id,
                finding.title,
                finding.severity,
                finding.blocking,
                span.map(|(offset, _)| offset),
                span.map(|(_, length)| length),
                evidence.as_ref().map(ReportHash::as_str),
            ])?;
        }
        write_sections(db, run, receipt, index.unwrap_or_default())?;
        edits::write_changes(db, run, receipt, &self.ledger.changes)?;
        db.execute(
            "UPDATE report_handoffs SET edits=json_patch(json_remove(edits,'$.structured.findings'),?2)
             WHERE receipt_id=?1",
            params![receipt, self.metadata],
        )?;
        Ok(())
    }
}

/// Whether a handoff's metadata fits its budget once acceptance replaces the submitted
/// cards with `accepted`, the serialized details (supplied and carried from the parent)
/// and carry origins. Staging passes the unsaved metadata; acceptance passes the saved row.
pub(super) fn fits(db: &Connection, edits: &str, accepted: &str) -> Result<bool, StoreError> {
    Ok(db.query_row(
        "SELECT length(CAST(json_patch(json_remove(?1,'$.structured.findings'),?2) AS BLOB))<=?3",
        params![edits, accepted, METADATA_LIMIT],
        |row| row.get(0),
    )?)
}

/// The accepted details and carry origins an amendment keeps from the version it amends.
pub(super) fn inherited(db: &Connection, parent: &str) -> Result<String, StoreError> {
    Ok(db.query_row(
        "SELECT json_object('acceptedDetails', json(json_extract(edits,'$.acceptedDetails')),
                            'carry', json(json_extract(edits,'$.carry')))
         FROM report_handoffs WHERE receipt_id=?1",
        [parent],
        |row| row.get(0),
    )?)
}

/// Copies the amended version's findings, sections and outcomes with the inherited metadata.
pub(super) fn copy_parent(
    db: &Connection,
    run: &str,
    parent: &str,
    receipt: &str,
    inherited: &str,
) -> Result<(), StoreError> {
    for statement in [
        "INSERT INTO report_findings
         SELECT run_id,?3,finding_id,title,severity,blocking,scalar_offset,scalar_length,evidence_sha256
         FROM report_findings WHERE run_id=?1 AND receipt_id=?2",
        "INSERT INTO report_sections
         SELECT run_id,?3,scalar_offset,scalar_length,title,evidence_sha256
         FROM report_sections WHERE run_id=?1 AND receipt_id=?2",
        "INSERT INTO report_changes
         SELECT run_id,?3,finding_id,conclusion,reason,checked_sha
         FROM report_changes WHERE run_id=?1 AND receipt_id=?2",
        "INSERT INTO report_change_discussions
         SELECT run_id,?3,finding_id,discussion_id
         FROM report_change_discussions WHERE run_id=?1 AND receipt_id=?2",
    ] {
        db.execute(statement, params![run, parent, receipt])?;
    }
    db.execute(
        "UPDATE report_handoffs SET edits=json_patch(edits,?2) WHERE receipt_id=?1",
        params![receipt, inherited],
    )?;
    Ok(())
}

fn authored_digest(report: &StructuredReport, id: &str) -> Result<Option<ReportHash>, StoreError> {
    report
        .findings
        .iter()
        .find(|finding| finding.id == id)
        .map(|finding| {
            let authored = serde_json::to_vec(finding).map_err(|_| AppError::Internal)?;
            Ok(ReportHash::from_sha256(Sha256::digest(authored).into()))
        })
        .transpose()
}

fn write_sections(
    db: &Connection,
    run: &str,
    receipt: &str,
    index: &[Descriptor],
) -> Result<(), StoreError> {
    let mut section = db.prepare_cached(
        "INSERT INTO report_sections(run_id, receipt_id, title, scalar_offset, scalar_length, evidence_sha256)
         VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    for entry in index {
        if let Descriptor::Section { title, source, .. } = entry {
            section.execute(params![
                run,
                receipt,
                title,
                db_int(source.offset)?,
                db_int(source.length)?,
                source.hash.as_str()
            ])?;
        }
    }
    Ok(())
}
