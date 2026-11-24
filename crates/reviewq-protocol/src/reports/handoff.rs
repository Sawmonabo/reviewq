use serde::{Deserialize, Serialize};

use super::ReportHashDto;
use crate::{GitShaDto, HistoryTurnIdDto, ResourceIdDto, SessionKeyDto};

pub mod limits;
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
pub const REPORT_HANDOFF_DESCRIPTION: &str = "Submit the finished review once, after following the repository's own review instructions and output format; do not change the review for ReviewQ. Point source at the exact report (an absolute file path, or response for your final message) and give the reviewed SHA, or {sha: null, reason}, and the verdict. In report.findings list every finding the report raises (blocking and nonblocking, functional, security and quality) with its ID, title, severity, blocking, consequence, evidence or evidence gap, and location when applicable. Use the report's own IDs (F-01, F-02, ...) or assign them; keep an ID for the same concern across versions and give a new concern the next unused ID. findings: [] means nothing was found. In findingChanges give an outcome for each finding of the last accepted report that you re-assessed: fixed, clarified_withdrawn or superseded retire it; partly_addressed, still_present or unverified keep it. A full review must list or retire every finding of the last accepted report. In a selected-thread follow-up add one assessment per listed discussion; findings you did not re-assess are carried unchanged. amendsReport records a publication edit on the current report and omits report. Use unchanged to keep the accepted report, or needs_input with a reason. If ReviewQ returns field errors, fix them and submit again in this turn.";

pub const REPORT_AMENDMENT_SOURCE_MESSAGE: &str = "amendsReport keeps the accepted report's exact bytes. For changed content, submit a final with its findings and reviewed SHA.";

/// Tool input and the exact JSON file accepted by the native CLI.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportHandoffInput {
    #[serde(deserialize_with = "require_structured_final")]
    pub handoff: Handoff,
}

/// A final carries its findings ledger unless it amends the accepted report.
fn require_structured_final<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Handoff, D::Error> {
    let handoff = Handoff::deserialize(deserializer)?;
    match &handoff {
        Handoff::Final {
            report: None,
            amends: None,
            ..
        } => Err(serde::de::Error::missing_field("report")),
        Handoff::Final {
            report: Some(_),
            amends: Some(_),
            ..
        } => Err(serde::de::Error::custom(
            "Omit report when amendsReport is present.",
        )),
        _ => Ok(handoff),
    }
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
    #[serde(deserialize_with = "require_structured_final")]
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
        /// Required unless `amendsReport` is present, and omitted when it is.
        #[serde(skip_serializing_if = "Option::is_none")]
        report: Option<StructuredReport>,
        #[serde(rename = "amendsReport", skip_serializing_if = "Option::is_none")]
        amends: Option<AcceptedReference>,
        #[serde(
            rename = "findingChanges",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        #[cfg_attr(feature = "schema", schemars(length(max = limits::MAX_FINDING_CHANGES)))]
        finding_changes: Vec<FindingEdit>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "schema", schemars(length(max = limits::MAX_ASSESSMENTS)))]
        assessments: Vec<Assessment>,
    },
    Unchanged {
        accepted: AcceptedReference,
        source: ReportSource,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "schema", schemars(length(max = limits::MAX_ASSESSMENTS)))]
        assessments: Vec<Assessment>,
    },
    NeedsInput {
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = limits::MAX_REASON_BYTES)))]
        reason: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "schema", schemars(length(max = limits::MAX_ASSESSMENTS)))]
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
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = limits::MAX_REASON_BYTES)))]
    pub reason: String,
    #[cfg_attr(feature = "schema", schemars(length(max = limits::MAX_ASSESSMENT_FINDING_IDS)))]
    pub finding_ids: Vec<ResourceIdDto>,
    pub next_action: crate::followups::NextAction,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingEdit {
    pub finding_id: ResourceIdDto,
    pub conclusion: super::FindingConclusion,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = limits::MAX_REASON_BYTES)))]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "schema", schemars(length(max = limits::MAX_DISCUSSION_IDS)))]
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
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = limits::MAX_REASON_BYTES)))]
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
