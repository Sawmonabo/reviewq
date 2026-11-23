use std::collections::{BTreeMap, BTreeSet};

use reviewq_app::access::AppError;
use reviewq_app::reports::{
    CorrectionKind, Descriptor, EvidenceTarget, FindingDetails, ReportEdits, ReportScope,
    RevisionEvidence, SourceRange, StructuredFinding, StructuredReport, presentation_source,
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
    promoted: BTreeMap<String, SourceRange>,
}

#[derive(Serialize)]
struct AcceptedMetadata {
    #[serde(rename = "acceptedDetails")]
    details: BTreeMap<String, FindingDetails>,
    #[serde(skip_serializing_if = "carry::Carry::is_empty")]
    carry: carry::Carry,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    db: &Connection,
    run: &str,
    previous: Option<&str>,
    same_source: bool,
    report: &StructuredReport,
    revision: &RevisionEvidence,
    edits: &ReportEdits,
    sections: &[Descriptor],
) -> Result<Option<Projection>, StoreError> {
    let retain = report.scope != ReportScope::FullReview;
    let previous = previous.filter(|_| retain);
    let mut prior = BTreeMap::new();
    let mut details: BTreeMap<String, FindingDetails> = BTreeMap::new();
    let mut promoted = BTreeMap::new();
    if let Some(receipt) = previous {
        let mut statement = db.prepare_cached(
            "SELECT finding_id, evidence_sha256 FROM report_findings
             WHERE run_id=?1 AND receipt_id=?2 ORDER BY finding_id",
        )?;
        let rows = statement.query_map(params![run, receipt], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
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
    if retain {
        let supplied = report
            .findings
            .iter()
            .map(|finding| finding.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut expected = BTreeSet::new();
        for edit in &edits.findings {
            if edit.conclusion.retires_finding() {
                if !prior.contains_key(&edit.finding_id) {
                    return Ok(None);
                }
            } else {
                expected.insert(edit.finding_id.as_str());
            }
        }
        if let Some(correction) = &edits.correction {
            let Some(previous) = previous else {
                return Ok(None);
            };
            let mut cited_findings = BTreeSet::new();
            let mut cited_sections = BTreeSet::new();
            for reference in &correction.evidence {
                let matched = match &reference.target {
                    EvidenceTarget::Finding(id) => {
                        cited_findings.insert(id.as_str())
                            && prior.get(id).and_then(Option::as_deref)
                                == Some(reference.hash.as_str())
                    }
                    EvidenceTarget::Section(title) => {
                        cited_sections.insert((title.as_str(), reference.hash.as_str()))
                            && carry::matches_section(
                                db,
                                run,
                                previous,
                                title,
                                reference.hash.as_str(),
                            )?
                    }
                };
                if !matched {
                    return Ok(None);
                }
            }
            let new_ids = correction
                .finding_ids
                .iter()
                .map(String::as_str)
                .filter(|id| !prior.contains_key(*id))
                .collect::<BTreeSet<_>>();
            if new_ids.len() > cited_sections.len()
                || correction
                    .finding_ids
                    .iter()
                    .any(|id| prior.contains_key(id) && !cited_findings.contains(id.as_str()))
            {
                return Ok(None);
            }
            if correction.kind == CorrectionKind::Presentation {
                if !same_source {
                    return Ok(None);
                }
                expected.extend(new_ids);
                let mut spans = BTreeSet::new();
                for finding in &report.findings {
                    let Some(source) = presentation_source(finding, correction, sections) else {
                        return Ok(None);
                    };
                    if !spans.insert(source.offset)
                        || prior
                            .values()
                            .any(|hash| hash.as_deref() == Some(source.hash.as_str()))
                    {
                        return Ok(None);
                    }
                    promoted.insert(finding.id.clone(), source.clone());
                }
            }
        }
        if supplied != expected {
            return Ok(None);
        }
        for edit in &edits.sections {
            let Some(previous) = previous else {
                return Ok(None);
            };
            if !carry::matches_section(db, run, previous, &edit.baseline_title, edit.baseline_hash.as_str())?
                || edit.replacement_title.as_ref().is_some_and(|expected| {
                    sections.iter().filter(|entry| matches!(entry, Descriptor::Section { title, .. } if title == expected)).count() != 1
                })
            {
                return Ok(None);
            }
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
        count += usize::from(!prior.contains_key(&finding.id));
        let mut current = finding.details.clone();
        current.checked_sha = checked_sha.map(str::to_owned);
        details.insert(finding.id.clone(), current);
        replaced.push(finding.id.clone());
    }
    let carry = carry::prepare(db, run, previous, edits, sections)?;
    let metadata = serde_json::to_string(&AcceptedMetadata { details, carry })
        .map_err(|_| AppError::Internal)?;
    Ok(Some(Projection {
        count: count as i64,
        retain,
        replaced,
        metadata,
        promoted,
    }))
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
            "INSERT INTO report_findings(run_id,receipt_id,finding_id,title,severity,blocking,evidence_sha256,scalar_offset,scalar_length)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?;
        for finding in supplied {
            let authored = serde_json::to_vec(finding).map_err(|_| AppError::Internal)?;
            let digest = ReportHash::from_sha256(Sha256::digest(authored).into());
            let source = self.promoted.get(&finding.id);
            insert.execute(params![
                run,
                receipt,
                finding.id,
                finding.title,
                finding.severity,
                finding.blocking,
                source.map_or(digest.as_str(), |source| source.hash.as_str()),
                source
                    .map(|source| super::db_int(source.offset))
                    .transpose()?,
                source
                    .map(|source| super::db_int(source.length))
                    .transpose()?,
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

#[cfg(test)]
mod tests {
    use reviewq_app::reports::{FindingBasis, FindingBasisKind, FindingConclusion, FindingEdit};
    use serde_json::json;

    use super::*;

    #[test]
    fn scoped_change_preserves_untouched_card_and_its_checked_revision() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        db.execute_batch(include_str!("../../schema/reports.sql"))
            .unwrap();
        let prior_sha = "a".repeat(40);
        let next_sha = "b".repeat(40);
        let original = StructuredReport {
            version: 1,
            scope: ReportScope::FullReview,
            findings: ["duplicate-invoice", "late-reminder"]
                .into_iter()
                .map(finding)
                .collect(),
        };
        let revised = StructuredReport {
            version: 1,
            scope: ReportScope::SelectedThreads,
            findings: Vec::new(),
        };
        for (position, (receipt, report)) in [("first", &original), ("next", &revised)]
            .into_iter()
            .enumerate()
        {
            db.execute(
                "INSERT INTO report_handoffs(receipt_id,run_id,lead_session_key,turn_id,call_id,
                    source_generation,source_position,request_sha256,declaration_sha256,form,state,
                    source_kind,verdict,disposition_version,due_at_ms,created_at_ms,updated_at_ms,
                    settings_revision,edits)
                 VALUES(?1,'run','codex:lead',?1,?1,'gen',?2,?3,?3,'final','pending_completion',
                   'response','request_changes',0,0,0,0,0,?4)",
                params![
                    receipt,
                    position as i64,
                    "c".repeat(64),
                    json!({"structured":report}).to_string()
                ],
            )
            .unwrap();
        }
        let initial = prepare(
            &db,
            "run",
            None,
            false,
            &original,
            &RevisionEvidence::Exact(prior_sha.parse().unwrap()),
            &ReportEdits::default(),
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(initial.count, 2);
        assert!(initial.fits(&db, "first").unwrap());
        initial
            .install(&db, "run", "first", None, &original.findings)
            .unwrap();

        let revision = RevisionEvidence::Exact(next_sha.parse().unwrap());
        let mut edits = ReportEdits {
            findings: vec![FindingEdit {
                finding_id: "duplicate-invoice".into(),
                conclusion: FindingConclusion::Fixed,
                reason: "Retry now retains the invoice key.".into(),
                checked_sha: next_sha.parse().unwrap(),
                discussion_ids: vec!["thread-1".into()],
            }],
            ..ReportEdits::default()
        };
        let result = prepare(
            &db,
            "run",
            Some("first"),
            false,
            &revised,
            &revision,
            &edits,
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.count, 1);
        assert!(result.fits(&db, "next").unwrap());
        result
            .install(&db, "run", "next", Some("first"), &revised.findings)
            .unwrap();
        let stored: (String, String, String) = db.query_row(
            "SELECT f.finding_id, json_extract(h.edits,'$.acceptedDetails.\"late-reminder\".checkedSha'),
                json_extract(h.edits,'$.acceptedDetails.\"late-reminder\".evidence.text')
             FROM report_findings f JOIN report_handoffs h USING(receipt_id) WHERE f.receipt_id='next'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(
            stored,
            (
                "late-reminder".into(),
                prior_sha,
                "Observed in the test trace.".into()
            )
        );
        let old_count: i64 = db
            .query_row(
                "SELECT count(*) FROM report_findings WHERE receipt_id='first'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_count, 2);

        assert_mixed_legacy_source(&db, &revised, &revision, &edits);

        edits.findings[0].conclusion = FindingConclusion::StillPresent;
        assert!(
            prepare(
                &db,
                "run",
                Some("first"),
                false,
                &revised,
                &revision,
                &edits,
                &[]
            )
            .unwrap()
            .is_none()
        );
    }

    fn assert_mixed_legacy_source(
        db: &Connection,
        revised: &StructuredReport,
        revision: &RevisionEvidence,
        edits: &ReportEdits,
    ) {
        use reviewq_app::reports::{Collection, ContextRequest};

        let prior_sha = "a".repeat(40);
        let next_sha = "b".repeat(40);
        let previous_hash = "d".repeat(64);
        let next_hash = "e".repeat(64);
        let evidence_hash = "f".repeat(64);
        for (receipt, reviewed, hash, handoff) in [
            ("legacy", &prior_sha, &previous_hash, None),
            ("next-legacy", &next_sha, &next_hash, Some("next-legacy")),
        ] {
            db.execute(
                "INSERT INTO artifacts(artifact_id,sha256,byte_length,total_code_points,line_count,
                                       max_line_bytes,complete,sealed,created_at_ms)
                 VALUES(?1,?2,100,100,5,40,1,1,0)",
                params![receipt, hash],
            )
            .unwrap();
            db.execute(
                "INSERT INTO report_versions(run_id,receipt_id,artifact_id,handoff_receipt_id,attention_receipt_id,
                      reviewed_sha,coverage,verdict,semantic_index,finding_count,change_count,captured_at_ms,
                      starting_disposition_version)
                 VALUES('run',?1,?1,?3,?1,?2,'full_mr','request_changes','ready',2,0,0,0)",
                params![receipt, reviewed, handoff],
            ).unwrap();
        }
        db.execute(
            "INSERT INTO report_handoffs(receipt_id,run_id,lead_session_key,turn_id,call_id,
                source_generation,source_position,request_sha256,declaration_sha256,form,state,
                source_kind,verdict,disposition_version,due_at_ms,created_at_ms,updated_at_ms,settings_revision,edits)
             VALUES('next-legacy','run','codex:lead','next-legacy','next-legacy','gen',12,?1,?1,
                'final','pending_completion','response','request_changes',0,0,0,0,0,?2)",
            params![evidence_hash, json!({"structured":revised}).to_string()],
        ).unwrap();
        for (finding, offset) in [("duplicate-invoice", 10_i64), ("late-reminder", 30)] {
            db.execute("INSERT INTO report_findings VALUES('run','legacy',?1,'Legacy evidence','high',0,?2,10,?3)",
                params![finding, offset, evidence_hash]).unwrap();
        }
        db.execute("INSERT INTO report_sections VALUES('run','legacy',0,100,'Earlier report',?1),
            ('run','legacy',10,10,'Fixed evidence',?1),('run','legacy',60,10,'Unreviewed production configuration',?1),
            ('run','next-legacy',0,10,'New report',?2)", params![evidence_hash, next_hash]).unwrap();
        let projection = prepare(
            db,
            "run",
            Some("legacy"),
            false,
            revised,
            revision,
            edits,
            &[],
        )
        .unwrap()
        .unwrap();
        projection
            .install(db, "run", "next-legacy", Some("legacy"), &[])
            .unwrap();
        let read = |collection, after, limit| {
            crate::reports::context::read(
                db,
                &"run".parse().unwrap(),
                ContextRequest {
                    receipt: Some("next-legacy".parse().unwrap()),
                    collection,
                    after,
                    limit,
                },
            )
            .unwrap()
        };
        let findings = read(Collection::Findings, None, 5);
        let [
            Descriptor::Finding {
                id,
                source: Some(source),
                ..
            },
        ] = findings.items.as_slice()
        else {
            panic!("expected one carried legacy finding")
        };
        assert_eq!(id, "late-reminder");
        assert_eq!(source.offset, 30);
        let origin = source.origin.as_ref().unwrap();
        assert_eq!(origin.report.receipt_id.as_str(), "legacy");
        assert_eq!(origin.report.hash.as_str(), previous_hash);
        assert_eq!(origin.checked_sha.as_ref().unwrap().as_str(), prior_sha);

        let first_page = read(Collection::Sections, None, 1);
        let sections = read(Collection::Sections, first_page.next, 5).items;
        assert_eq!(sections.len(), 2);
        let Descriptor::Section { title, source, .. } = &sections[1] else {
            panic!("expected a carried standalone section")
        };
        assert_eq!(title, "Unreviewed production configuration");
        assert_eq!(source.offset, 60);
        assert_eq!(
            source.origin.as_ref().unwrap().report.receipt_id.as_str(),
            "legacy"
        );

        let section = "### Legacy evidence\n\nSeverity: high\nBlocking: false\nEvidence: Observed in the test trace.\n\nNo duplicate charges occur.\n";
        let mut proposal = finding("promotion");
        proposal.details.consequence = "No duplicate charges occur.".into();
        assert!(reviewq_app::reports::presentation_matches(
            section, &proposal
        ));
        proposal.details.consequence = "duplicate charges occur.".into();
        assert!(!reviewq_app::reports::presentation_matches(
            section, &proposal
        ));
    }

    fn finding(id: &str) -> StructuredFinding {
        StructuredFinding {
            id: id.into(),
            title: "Unexpected workflow result".into(),
            severity: "high".into(),
            blocking: false,
            details: FindingDetails {
                consequence: "The user cannot trust the displayed state.".into(),
                location: None,
                evidence: FindingBasis {
                    kind: FindingBasisKind::Observed,
                    text: "Observed in the test trace.".into(),
                },
                checked_sha: None,
                evidence_hash: None,
            },
        }
    }
}
