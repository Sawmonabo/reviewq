//! What each reviewing tool returns on success.

use serde::{Deserialize, Serialize};

use super::NoteSection;
use crate::followups::ResultKind;
use crate::reports::{
    FindingBasis, FindingConclusion, FindingKind, FindingLocation, ReportHashDto, Severity,
};
use crate::{DiscussionIdDto, FindingIdDto, GitShaDto, PageCursorDto, ResourceIdDto};

/// The result of `save_finding` and `set_finding_outcome`: `{id,revision,unpublished}`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingReceipt {
    pub id: FindingIdDto,
    /// The finding's revision after this call. An unchanged write keeps the stored one.
    pub revision: u64,
    /// How many findings and sections changed since the last published version.
    pub unpublished: u64,
}

/// The result of `save_section`: `{section,title?,revision,unpublished}`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteReceipt {
    pub section: NoteSection,
    /// The extra section's title. Absent for every other section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub revision: u64,
    /// How many findings and sections changed since the last published version.
    pub unpublished: u64,
}

/// The result of `read_draft`: the short list for an empty input, otherwise whole records.
/// Serialize only: the two shapes differ by fields, not by a tag.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum LedgerPage {
    Summary(LedgerSummary),
    Records(LedgerRecords),
}

/// The short list: the current version, the open pass, one line per active finding and saved section.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerSummary {
    /// The current version's receipt, or null before the first version.
    pub receipt_id: Option<ResourceIdDto>,
    /// The open pass, or null when none is open.
    pub pass: Option<LedgerPass>,
    pub findings: Vec<FindingLine>,
    pub sections: Vec<NoteLine>,
    /// Pass it back with the same input for the rest, or null when this page is the last.
    pub cursor: Option<PageCursorDto>,
}

/// Whole records, as many as fit one page.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerRecords {
    pub findings: Vec<FindingRecord>,
    pub sections: Vec<NoteRecord>,
    /// Pass it back with the same input for the rest, or null when this page is the last.
    pub cursor: Option<PageCursorDto>,
}

/// The kind of review pass a run has open.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassKind {
    FullReview,
    SelectedThreads,
}

/// The open pass and what its closing call owes.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerPass {
    pub kind: PassKind,
    /// True while `wait_for_user` paused the pass and it waits on the user's reply.
    pub waiting: bool,
    /// Plain sentences naming what the closing call must supply and the question the pass
    /// last asked with `wait_for_user`, if any.
    pub owes: String,
    /// The selected discussions a selected-thread pass needs one assessment for each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discussion_ids: Vec<DiscussionIdDto>,
}

/// One active finding in the short list.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingLine {
    pub id: FindingIdDto,
    pub revision: u64,
    pub severity: Severity,
    pub blocking: bool,
    pub title: String,
    /// `lead`, or the agent ID of the subagent that last changed it.
    pub author: String,
}

/// One saved section in the short list.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteLine {
    pub section: NoteSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub revision: u64,
    /// The body's length in characters.
    pub length: u64,
}

/// A finding as stored, at its current revision.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingRecord {
    pub id: FindingIdDto,
    pub revision: u64,
    /// False once a retiring outcome took it out of the active set.
    pub active: bool,
    /// `lead`, or the agent ID of the subagent that last changed it.
    pub author: String,
    pub title: String,
    pub severity: Severity,
    pub blocking: bool,
    pub kind: FindingKind,
    pub category: String,
    pub locations: Vec<FindingLocation>,
    pub issue: String,
    pub consequence: String,
    pub evidence: FindingBasis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    /// The latest outcome `set_finding_outcome` recorded, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RecordedOutcome>,
}

/// An outcome recorded on a finding.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedOutcome {
    pub outcome: FindingConclusion,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discussion_ids: Vec<DiscussionIdDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_sha: Option<GitShaDto>,
}

/// A saved section, at its current revision.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRecord {
    pub section: NoteSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub revision: u64,
    pub body: String,
}

/// The result of `submit_review`:
/// `{receiptId,sha256,result,new,updated,retired,carried,bySubagents}`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitReceipt {
    /// The published version, or for an `unchanged` selected-thread result the current one.
    pub receipt_id: ResourceIdDto,
    /// SHA-256 of the version's composed text.
    pub sha256: ReportHashDto,
    pub result: ResultKind,
    pub new: Vec<FindingIdDto>,
    pub updated: Vec<FindingIdDto>,
    pub retired: Vec<FindingIdDto>,
    /// How many findings of the parent version stayed at their revision.
    pub carried: u64,
    /// The findings that subagents filed or changed in this version.
    pub by_subagents: Vec<FindingIdDto>,
}

/// The result of `wait_for_user`: the paused pass and what it keeps unpublished.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldReceipt {
    /// The pass that now waits on the user.
    pub pass: PassKind,
    /// How many findings and sections changed since the last published version.
    pub unpublished: u64,
}
