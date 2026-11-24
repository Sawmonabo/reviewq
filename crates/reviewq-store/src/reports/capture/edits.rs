use std::borrow::Cow;
use std::sync::Arc;

use reviewq_app::access::AppError;
use reviewq_app::reports::{FindingEdit, METADATA_BYTES, ReportEdits, StructuredReport};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::StoreError;
use crate::queue::parse;

#[derive(Serialize, Deserialize)]
struct StoredEdits<'a> {
    findings: Vec<StoredFinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    structured: Option<Cow<'a, StructuredReport>>,
}

#[derive(Serialize, Deserialize)]
struct StoredFinding {
    finding_id: String,
    conclusion: String,
    reason: String,
    checked_sha: Option<String>,
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
        findings: edits
            .findings
            .iter()
            .map(|item| StoredFinding {
                finding_id: item.finding_id.clone(),
                conclusion: item.conclusion.to_string(),
                reason: item.reason.clone(),
                checked_sha: item.checked_sha.as_ref().map(ToString::to_string),
                discussion_ids: item.discussion_ids.clone(),
            })
            .collect(),
    };
    let encoded = serde_json::to_string(&value).map_err(|_| AppError::Internal)?;
    if encoded.len() > METADATA_BYTES {
        return Err(AppError::PayloadTooLarge.into());
    }
    Ok(Some(encoded))
}

pub(super) fn decode(
    encoded: Option<&str>,
) -> Result<(ReportEdits, Option<Arc<StructuredReport>>), StoreError> {
    let Some(encoded) = encoded else {
        return Ok((ReportEdits::default(), None));
    };
    let value: StoredEdits<'_> = serde_json::from_str(encoded).map_err(|_| AppError::Internal)?;
    let edits = ReportEdits {
        findings: value
            .findings
            .into_iter()
            .map(|item| {
                Ok(FindingEdit {
                    finding_id: item.finding_id,
                    conclusion: parse(&item.conclusion)?,
                    reason: item.reason,
                    checked_sha: item.checked_sha.as_deref().map(parse).transpose()?,
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
            change.checked_sha.as_ref().map(|sha| sha.as_str())
        ])?;
        for id in &change.discussion_ids {
            link.execute(params![run, receipt, change.finding_id, id])?;
        }
    }
    Ok(())
}
