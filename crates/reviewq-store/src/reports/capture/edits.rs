use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use reviewq_app::access::AppError;
use reviewq_app::reports::{
    CapturedArtifact, Correction, CorrectionKind, Declaration, Descriptor, EvidenceReference,
    EvidenceTarget, FindingEdit, ReportEdits, ReportReference, ReportWork, SectionEdit,
    SourceRange, StructuredReport,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{batch, ensure_claim};

use crate::queue::parse;
use crate::{SqliteStore, StoreError};

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

pub(super) async fn verify(
    store: &SqliteStore,
    work: &ReportWork,
    artifact: Option<&mut CapturedArtifact>,
) -> Result<bool, AppError> {
    let Declaration::Final {
        edits,
        assessments,
        report,
        ..
    } = &work.declaration
    else {
        return Ok(true);
    };
    let structured = report.is_some();
    if edits.correction.is_none() && (structured || assessments.is_empty()) {
        return Ok(true);
    }
    let Some(artifact) = artifact else {
        return Ok(false);
    };
    if structured
        && edits
            .correction
            .as_ref()
            .is_some_and(|correction| correction.base.hash == artifact.hash)
    {
        return Ok(true);
    }
    let Some(index) = artifact.index.as_ref() else {
        return Ok(false);
    };
    let context = work.clone();
    let correction = edits
        .correction
        .as_ref()
        .map(|correction| correction.base.clone());
    let basis = store
        .read(move |db| {
            let base = match correction {
                Some(base) => Some(base),
                None => {
                    let scope = batch::manifest(db, &context)?.ok_or(AppError::Conflict)?;
                    match (scope.report_receipt_id, scope.report_hash) {
                        (Some(id), Some(hash)) => Some(ReportReference {
                            receipt_id: parse(&id)?,
                            hash: parse(&hash)?,
                        }),
                        (None, None) => None,
                        _ => return Err(AppError::Conflict.into()),
                    }
                }
            };
            let Some(base) = base else {
                return Ok(None);
            };
            Ok(Some(prepare(db, &context, &base, !structured)?))
        })
        .await
        .map_err(StoreError::app);
    let evidence = match basis {
        Ok(Some(evidence)) => evidence,
        Ok(None) => {
            return Ok(edits.sections.is_empty()
                && index.iter().all(|entry| match entry {
                    Descriptor::Finding { id, .. } => {
                        edits.findings.iter().any(|edit| &edit.finding_id == id)
                    }
                    _ => true,
                }));
        }
        Err(AppError::Conflict) => return Ok(false),
        Err(error) => return Err(error),
    };
    let index = index.to_vec();
    let edits = if structured {
        ReportEdits {
            sections: edits.sections.clone(),
            ..ReportEdits::default()
        }
    } else {
        edits.clone()
    };
    let replacement = tokio::task::spawn_blocking(move || validate_source(evidence, &edits, index))
        .await
        .map_err(|_| AppError::Internal)?;
    if let Some(index) = replacement {
        artifact.index = Some(index);
        return Ok(true);
    }
    Ok(false)
}

type Findings = BTreeMap<String, Descriptor>;

pub(super) struct SourceEvidence {
    before: String,
    after: String,
    findings: Findings,
    sections: Vec<Descriptor>,
}

fn prepare(
    db: &Connection,
    work: &ReportWork,
    reference: &ReportReference,
    include_findings: bool,
) -> Result<Option<SourceEvidence>, StoreError> {
    let run = work.call.lead.run_id().as_str();
    let receipt = reference.receipt_id.as_str();
    let previous = db.query_row(
        "SELECT a.artifact_id FROM report_versions v JOIN artifacts a USING(artifact_id)
         WHERE v.run_id=?1 AND v.receipt_id=?2 AND a.sha256=?3 AND a.sealed=1 AND a.complete=1
           AND (a.original_sha256 IS NULL OR a.original_sha256=a.sha256) AND v.semantic_index='ready'",
        params![run, receipt, reference.hash.as_str()], |row| row.get::<_, String>(0),
    ).optional()?;
    let (Some(previous), Some(candidate)) = (previous, ensure_claim(db, work)?) else {
        return Ok(None);
    };
    let (Some(before), Some(after)) = (
        batch::read_semantic_body(db, &previous)?,
        batch::read_semantic_body(db, &candidate)?,
    ) else {
        return Ok(None);
    };
    let mut findings = BTreeMap::new();
    let mut query = db.prepare_cached(
        "SELECT finding_id,title,severity,blocking,scalar_offset,scalar_length,evidence_sha256
         FROM report_findings WHERE run_id=?1 AND receipt_id=?2 AND ?3 ORDER BY finding_id",
    )?;
    let mut rows = query.query(params![run, receipt, include_findings])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let Some(offset) = row.get::<_, Option<i64>>(4)? else {
            return Ok(None);
        };
        findings.insert(
            id.clone(),
            Descriptor::Finding {
                id,
                title: row.get(1)?,
                severity: row.get(2)?,
                blocking: row.get(3)?,
                source: Some(SourceRange {
                    offset: u64::try_from(offset).map_err(|_| AppError::Internal)?,
                    length: u64::try_from(row.get::<_, i64>(5)?).map_err(|_| AppError::Internal)?,
                    hash: parse(crate::queue::text(row, 6)?)?,
                }),
                details: None,
                disposition: None,
            },
        );
    }
    let mut sections = Vec::new();
    let mut query = db.prepare_cached(
        "SELECT title,scalar_offset,scalar_length,evidence_sha256 FROM report_sections
         WHERE run_id=?1 AND receipt_id=?2 ORDER BY scalar_offset",
    )?;
    let mut rows = query.query(params![run, receipt])?;
    while let Some(row) = rows.next()? {
        sections.push(Descriptor::Section {
            title: row.get(0)?,
            source: SourceRange {
                offset: u64::try_from(row.get::<_, i64>(1)?).map_err(|_| AppError::Internal)?,
                length: u64::try_from(row.get::<_, i64>(2)?).map_err(|_| AppError::Internal)?,
                hash: parse(crate::queue::text(row, 3)?)?,
            },
        });
    }
    Ok(Some(SourceEvidence {
        before,
        after,
        findings,
        sections,
    }))
}

fn validate_source(
    input: Option<SourceEvidence>,
    edits: &ReportEdits,
    mut index: Vec<Descriptor>,
) -> Option<Vec<Descriptor>> {
    let input = input?;
    let mut citations = Vec::new();
    if let Some(correction) = &edits.correction {
        for evidence in &correction.evidence {
            let source = match &evidence.target {
                EvidenceTarget::Finding(id) => {
                    let source = finding_source(input.findings.get(id)?)?;
                    (source.hash == evidence.hash).then_some(source)?
                }
                EvidenceTarget::Section(title) => {
                    let (title, source) = section(&input.sections, title, Some(&evidence.hash))?;
                    citations.push((title.to_owned(), source.clone()));
                    source
                }
            };
            if source.length == 0 {
                return None;
            }
        }
        if correction.kind == CorrectionKind::Presentation {
            if input.before != input.after {
                return None;
            }
            let new_ids = correction
                .finding_ids
                .iter()
                .filter(|id| !input.findings.contains_key(*id))
                .collect::<Vec<_>>();
            if new_ids.len() != citations.len() {
                return None;
            }
            let mut authored = input.findings.into_values().collect::<Vec<_>>();
            for ((title, source), id) in citations.into_iter().zip(new_ids) {
                if authored
                    .iter()
                    .any(|entry| finding_source(entry) == Some(&source))
                {
                    return None;
                }
                authored.push(Descriptor::Finding {
                    id: id.to_owned(),
                    title,
                    severity: None,
                    blocking: None,
                    source: Some(source),
                    details: None,
                    disposition: None,
                });
            }
            index.retain(|entry| !matches!(entry, Descriptor::Finding { .. }));
            index.extend(authored);
            return Some(index);
        }
    }
    if let Some(correction) = &edits.correction {
        let cited = |id: &str| {
            correction.evidence.iter().any(|evidence|
            matches!(&evidence.target, EvidenceTarget::Finding(found) if found == id))
        };
        if correction.finding_ids.iter().any(|id| {
            if input.findings.contains_key(id) {
                !cited(id)
            } else {
                citations.is_empty()
            }
        }) {
            return None;
        }
    }
    let mut before = Vec::new();
    let mut after = Vec::new();
    let changes = edits
        .findings
        .iter()
        .map(|change| (change.finding_id.as_str(), change))
        .collect::<BTreeMap<_, _>>();
    let mut current = index
        .iter()
        .filter_map(|entry| match entry {
            Descriptor::Finding { id, .. } => Some((id.as_str(), entry)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut retained = Vec::new();
    for (id, entry) in &input.findings {
        let source = finding_source(entry)?;
        if let Some(change) = changes.get(id.as_str()) {
            before.push((source.offset, source.length));
            if let Some(next) = current.remove(id.as_str()) {
                let range = finding_source(next)?;
                after.push((range.offset, range.length));
            } else if !change.conclusion.retires_finding() {
                return None;
            }
        } else if let Some(next) = current.remove(id.as_str()) {
            if finding_source(next)?.hash != source.hash {
                return None;
            }
        } else {
            // A promoted card can retain its original exact section through a later version.
            let Descriptor::Finding {
                title,
                severity,
                blocking,
                ..
            } = entry
            else {
                return None;
            };
            let (_, moved) = section(&index, title, Some(&source.hash))?;
            retained.push(Descriptor::Finding {
                id: id.clone(),
                title: title.clone(),
                severity: severity.clone(),
                blocking: *blocking,
                source: Some(moved.clone()),
                details: None,
                disposition: None,
            });
        }
    }
    for (id, entry) in current {
        if !changes.contains_key(id) {
            return None;
        }
        let range = finding_source(entry)?;
        after.push((range.offset, range.length));
    }
    if edits.findings.iter().any(|edit| {
        !input.findings.contains_key(&edit.finding_id)
            && !index.iter().any(
                |entry| matches!(entry, Descriptor::Finding { id, .. } if id == &edit.finding_id),
            )
    }) {
        return None;
    }
    for change in &edits.sections {
        let (_, source) = section(
            &input.sections,
            &change.baseline_title,
            Some(&change.baseline_hash),
        )?;
        before.push((source.offset, source.length));
        if let Some(replacement) = &change.replacement_title {
            let (_, range) = section(&index, replacement, None)?;
            after.push((range.offset, range.length));
        }
    }
    if preserved_sections(&input.sections, &before) != preserved_sections(&index, &after)
        || batch::unaffected(&input.before, before)? != batch::unaffected(&input.after, after)?
    {
        return None;
    }
    index.extend(retained);
    Some(index)
}

fn preserved_sections<'a>(
    index: &'a [Descriptor],
    ranges: &[(u64, u64)],
) -> Vec<(&'a str, &'a reviewq_domain::identity::ReportHash)> {
    index
        .iter()
        .filter_map(|entry| match entry {
            Descriptor::Section { title, source }
                if !ranges.iter().any(|&(offset, length)| {
                    source.offset < offset.saturating_add(length)
                        && offset < source.offset.saturating_add(source.length)
                }) =>
            {
                Some((title.as_str(), &source.hash))
            }
            _ => None,
        })
        .collect()
}

fn finding_source(entry: &Descriptor) -> Option<&SourceRange> {
    match entry {
        Descriptor::Finding { source, .. } => source.as_ref(),
        _ => None,
    }
}

fn section<'a>(
    index: &'a [Descriptor],
    expected: &str,
    hash: Option<&reviewq_domain::identity::ReportHash>,
) -> Option<(&'a str, &'a SourceRange)> {
    let mut matches = index.iter().filter_map(|entry| match entry {
        Descriptor::Section { title, source }
            if title == expected && hash.is_none_or(|hash| hash == &source.hash) =>
        {
            Some((title.as_str(), source))
        }
        _ => None,
    });
    let item = matches.next()?;
    matches.next().is_none().then_some(item)
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

pub(super) fn overrides_user_blocking(
    db: &Connection,
    run: &str,
    disposition_version: i64,
    edits: &ReportEdits,
    artifact: &CapturedArtifact,
    structured: Option<&StructuredReport>,
) -> Result<bool, StoreError> {
    let authored = artifact.index.as_deref().unwrap_or_default();
    let mut current = db.prepare_cached(
        "SELECT removed=0 AND coalesce(blocking=0,0) FROM finding_dispositions
         WHERE run_id=?1 AND finding_id=?2 AND disposition_version<=?3
         ORDER BY disposition_version DESC LIMIT 1",
    )?;
    let legacy_ids = authored.iter().filter_map(|entry| match entry {
        Descriptor::Finding {
            id,
            blocking: Some(true),
            ..
        } if edits.findings.iter().any(|edit| &edit.finding_id == id) => Some(id),
        _ => None,
    });
    let structured_ids = structured.into_iter().flat_map(|report| {
        report
            .findings
            .iter()
            .filter(|item| item.blocking)
            .map(|item| &item.id)
    });
    for id in legacy_ids.chain(structured_ids) {
        if current
            .query_row(params![run, id, disposition_version], |row| {
                row.get::<_, bool>(0)
            })
            .optional()?
            .unwrap_or(false)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
