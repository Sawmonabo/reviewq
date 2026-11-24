use std::borrow::Cow;
use std::sync::Arc;

use reviewq_app::access::AppError;
use reviewq_app::reports::{
    Correction, EvidenceReference, EvidenceTarget, FindingEdit, ReportEdits, ReportReference,
    SectionEdit, StructuredReport,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::StoreError;
use crate::queue::parse;

#[derive(Serialize, Deserialize)]
struct StoredEdits<'a> {
    correction: Option<StoredCorrection>,
    findings: Vec<StoredFinding>,
    sections: Vec<StoredSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    structured: Option<Cow<'a, StructuredReport>>,
}

#[derive(Serialize, Deserialize)]
struct StoredCorrection {
    kind: String,
    reason: String,
    finding_ids: Vec<String>,
    evidence: Vec<StoredEvidence>,
}

#[derive(Serialize, Deserialize)]
enum StoredEvidence {
    Finding { id: String, hash: String },
    Section { title: String, hash: String },
}

#[derive(Serialize, Deserialize)]
struct StoredFinding {
    finding_id: String,
    conclusion: String,
    reason: String,
    checked_sha: String,
    discussion_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredSection {
    baseline_title: String,
    baseline_hash: String,
    replacement_title: Option<String>,
    conclusion: String,
    reason: String,
    discussion_ids: Vec<String>,
}

pub(super) fn encode(
    edits: &ReportEdits,
    structured: Option<&Arc<StructuredReport>>,
) -> Result<Option<String>, StoreError> {
    if edits.is_empty() && structured.is_none() {
        return Ok(None);
    }
    let value = StoredEdits {
        structured: structured.map(|report| Cow::Borrowed(report.as_ref())),
        correction: edits
            .correction
            .as_ref()
            .map(|correction| StoredCorrection {
                kind: correction.kind.to_string(),
                reason: correction.reason.clone(),
                finding_ids: correction.finding_ids.clone(),
                evidence: correction
                    .evidence
                    .iter()
                    .map(|item| match &item.target {
                        EvidenceTarget::Finding(id) => StoredEvidence::Finding {
                            id: id.clone(),
                            hash: item.hash.to_string(),
                        },
                        EvidenceTarget::Section(title) => StoredEvidence::Section {
                            title: title.clone(),
                            hash: item.hash.to_string(),
                        },
                    })
                    .collect(),
            }),
        findings: edits
            .findings
            .iter()
            .map(|item| StoredFinding {
                finding_id: item.finding_id.clone(),
                conclusion: item.conclusion.to_string(),
                reason: item.reason.clone(),
                checked_sha: item.checked_sha.to_string(),
                discussion_ids: item.discussion_ids.clone(),
            })
            .collect(),
        sections: edits
            .sections
            .iter()
            .map(|item| StoredSection {
                baseline_title: item.baseline_title.clone(),
                baseline_hash: item.baseline_hash.to_string(),
                replacement_title: item.replacement_title.clone(),
                conclusion: item.conclusion.to_string(),
                reason: item.reason.clone(),
                discussion_ids: item.discussion_ids.clone(),
            })
            .collect(),
    };
    let encoded = serde_json::to_string(&value).map_err(|_| AppError::Internal)?;
    if encoded.len() > 128 * 1024 {
        return Err(AppError::PayloadTooLarge.into());
    }
    Ok(Some(encoded))
}

pub(super) fn decode(
    encoded: Option<&str>,
    base: Option<&ReportReference>,
) -> Result<(ReportEdits, Option<Arc<StructuredReport>>), StoreError> {
    let Some(encoded) = encoded else {
        return Ok((ReportEdits::default(), None));
    };
    let value: StoredEdits<'_> = serde_json::from_str(encoded).map_err(|_| AppError::Internal)?;
    let edits = ReportEdits {
        correction: value
            .correction
            .map(|correction| {
                Ok::<_, StoreError>(Correction {
                    base: base.cloned().ok_or(AppError::Internal)?,
                    kind: parse(&correction.kind)?,
                    reason: correction.reason,
                    finding_ids: correction.finding_ids,
                    evidence: correction
                        .evidence
                        .into_iter()
                        .map(|item| match item {
                            StoredEvidence::Finding { id, hash } => Ok(EvidenceReference {
                                target: EvidenceTarget::Finding(id),
                                hash: parse(&hash)?,
                            }),
                            StoredEvidence::Section { title, hash } => Ok(EvidenceReference {
                                target: EvidenceTarget::Section(title),
                                hash: parse(&hash)?,
                            }),
                        })
                        .collect::<Result<_, StoreError>>()?,
                })
            })
            .transpose()?,
        findings: value
            .findings
            .into_iter()
            .map(|item| {
                Ok(FindingEdit {
                    finding_id: item.finding_id,
                    conclusion: parse(&item.conclusion)?,
                    reason: item.reason,
                    checked_sha: parse(&item.checked_sha)?,
                    discussion_ids: item.discussion_ids,
                })
            })
            .collect::<Result<_, StoreError>>()?,
        sections: value
            .sections
            .into_iter()
            .map(|item| {
                Ok(SectionEdit {
                    baseline_title: item.baseline_title,
                    baseline_hash: parse(&item.baseline_hash)?,
                    replacement_title: item.replacement_title,
                    conclusion: parse(&item.conclusion)?,
                    reason: item.reason,
                    discussion_ids: item.discussion_ids,
                })
            })
            .collect::<Result<_, StoreError>>()?,
    };
    Ok((
        edits,
        value.structured.map(|report| Arc::new(report.into_owned())),
    ))
}

pub(super) fn write_changes(
    db: &Connection,
    run: &str,
    receipt: &str,
    changes: &[FindingEdit],
) -> Result<(), StoreError> {
    let mut insert = db.prepare_cached(
        "INSERT INTO report_changes(run_id,receipt_id,finding_id,conclusion,reason,checked_sha)
         VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    let mut link = db.prepare_cached(
        "INSERT INTO report_change_discussions(run_id,receipt_id,finding_id,discussion_id) VALUES(?1,?2,?3,?4)",
    )?;
    for change in changes {
        insert.execute(params![
            run,
            receipt,
            change.finding_id,
            change.conclusion.to_string(),
            change.reason,
            change.checked_sha.as_str()
        ])?;
        for id in &change.discussion_ids {
            link.execute(params![run, receipt, change.finding_id, id])?;
        }
    }
    Ok(())
}
