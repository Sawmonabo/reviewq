use serde::{Deserialize, Serialize};

use super::ReportHashDto;
use crate::{GitShaDto, HistoryTurnIdDto, ResourceIdDto, SessionKeyDto};

mod structured;
mod validate;
pub use structured::*;
pub use validate::{
    ReportHandoffFieldError, report_amendment_source_error, validate_report_handoff,
};

pub const REPORT_HANDOFF_SCHEMA: &str =
    include_str!("../../../../contracts/report-handoff.schema.json");
pub const REPORT_HANDOFF_EXAMPLE: &str =
    include_str!("../../../../fixtures/contracts/report-handoff.json");
pub const REPORT_HANDOFF_DESCRIPTION: &str = "Submit the exact Markdown report or final native response and versioned finding cards for this linked ReviewQ lead. Set a reviewed SHA or an explicit unknown reason and a verdict. An empty findings list is valid. Use amendsReport for disposition or publication updates that preserve the accepted Markdown, revision, and verdict. For changed content with the same reviewed revision and verdict, use correctsReport with kind evidence and cite the accepted report. For a fresh review, use full_review or a bound selected_threads follow-up and supply the reviewed SHA. Selected-thread results include only findings updated by findingChanges and assessments of the frozen discussions. Unchanged retains the accepted receipt. Needs input records the reason. ReviewQ verifies native completion before accepting the report.";

pub const REPORT_AMENDMENT_SOURCE_MESSAGE: &str = "amendsReport requires the accepted Markdown bytes. Use a correctsReport evidence correction for changed content with the same reviewed revision and verdict. For a fresh assessment, submit a review using its reviewed SHA.";

/// Tool input and the exact JSON file accepted by the native CLI.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportHandoffInput {
    #[serde(deserialize_with = "require_structured_final")]
    pub handoff: Handoff,
}

fn require_structured_final<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Handoff, D::Error> {
    let handoff = Handoff::deserialize(deserializer)?;
    if matches!(handoff, Handoff::Final { report: None, .. }) {
        return Err(serde::de::Error::missing_field("report"));
    }
    Ok(handoff)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeReportAddress {
    pub session_key: SessionKeyDto,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum HandoffCommand {
    Codex(CodexCommand),
    Companion(CompanionCommand),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexCommand {
    pub session_key: SessionKeyDto,
    pub call_id: ResourceIdDto,
    pub handoff: Handoff,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanionCommand {
    pub context: NativeContext,
    #[serde(deserialize_with = "require_structured_final")]
    pub handoff: Handoff,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeContext {
    pub session_key: SessionKeyDto,
    pub turn_id: HistoryTurnIdDto,
    pub call_id: ResourceIdDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness_ordinal: Option<std::num::NonZeroU32>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Handoff {
    Final {
        source: ReportSource,
        revision: ReportRevision,
        verdict: ReportVerdict,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "schema", schemars(required))]
        report: Option<StructuredReport>,
        #[serde(rename = "amendsReport", skip_serializing_if = "Option::is_none")]
        amends: Option<AcceptedReference>,
        #[serde(rename = "correctsReport", skip_serializing_if = "Option::is_none")]
        corrects: Option<Box<Correction>>,
        #[serde(
            rename = "findingChanges",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        #[cfg_attr(feature = "schema", schemars(length(max = 50)))]
        finding_changes: Vec<FindingEdit>,
        #[serde(
            rename = "sectionChanges",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        #[cfg_attr(feature = "schema", schemars(length(max = 20)))]
        section_changes: Vec<SectionEdit>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "schema", schemars(length(max = 10)))]
        assessments: Vec<Assessment>,
    },
    Unchanged {
        accepted: AcceptedReference,
        source: ReportSource,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "schema", schemars(length(max = 10)))]
        assessments: Vec<Assessment>,
    },
    NeedsInput {
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        reason: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "schema", schemars(length(max = 10)))]
        assessments: Vec<Assessment>,
    },
}

impl Handoff {
    /// Whether a supplied file digest contradicts a same-source amendment.
    pub fn changes_amended_source(&self) -> bool {
        matches!(
            self,
            Self::Final {
                source: ReportSource::File { sha256: Some(file), .. },
                amends: Some(accepted),
                ..
            } if file != &accepted.sha256
        )
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Assessment {
    pub discussion_id: ResourceIdDto,
    pub conclusion: super::FindingConclusion,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
    pub reason: String,
    #[cfg_attr(feature = "schema", schemars(length(max = 50)))]
    pub finding_ids: Vec<ResourceIdDto>,
    pub next_action: crate::followups::NextAction,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Correction {
    pub receipt_id: ResourceIdDto,
    pub sha256: ReportHashDto,
    pub kind: CorrectionKind,
    pub reason: String,
    pub finding_ids: Vec<ResourceIdDto>,
    pub evidence: Vec<EvidenceReference>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CorrectionKind {
    Presentation,
    Evidence,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EvidenceReference {
    Finding(FindingEvidence),
    Section(SectionEvidence),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingEvidence {
    pub finding_id: ResourceIdDto,
    pub sha256: ReportHashDto,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SectionEvidence {
    pub section_title: String,
    pub sha256: ReportHashDto,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingEdit {
    pub finding_id: ResourceIdDto,
    pub conclusion: super::FindingConclusion,
    pub reason: String,
    pub checked_sha: GitShaDto,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discussion_ids: Vec<ResourceIdDto>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SectionEdit {
    pub baseline_title: String,
    pub baseline_sha256: ReportHashDto,
    pub replacement_title: Option<String>,
    pub conclusion: super::FindingConclusion,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discussion_ids: Vec<ResourceIdDto>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReportSource {
    File {
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 4096)))]
        #[cfg_attr(feature = "schema", schemars(regex(pattern = "^/")))]
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha256: Option<ReportHashDto>,
    },
    Response,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReportRevision {
    Exact {
        sha: GitShaDto,
    },
    Unknown {
        sha: (),
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        reason: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReportVerdict {
    Approve,
    RequestChanges,
    Blocked,
    NotSpecified,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AcceptedReference {
    pub receipt_id: ResourceIdDto,
    pub sha256: ReportHashDto,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedReport {
    pub receipt_id: ResourceIdDto,
    pub sha256: ReportHashDto,
    pub source: Option<ReportSource>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffState {
    PendingCompletion,
    Recorded,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffReceipt {
    pub receipt_id: ResourceIdDto,
    pub state: HandoffState,
    pub declaration_sha256: ReportHashDto,
}
