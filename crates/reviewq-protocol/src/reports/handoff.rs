use serde::{Deserialize, Serialize};

use super::ReportHashDto;
use crate::{GitShaDto, HistoryTurnIdDto, ResourceIdDto, SessionKeyDto};

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
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
    pub handoff: Handoff,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeContext {
    pub session_key: SessionKeyDto,
    pub turn_id: HistoryTurnIdDto,
    pub call_id: ResourceIdDto,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum Handoff {
    Final {
        source: ReportSource,
        revision: ReportRevision,
        verdict: ReportVerdict,
        #[serde(rename = "amendsReport", skip_serializing_if = "Option::is_none")]
        amends: Option<AcceptedReference>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        assessments: Vec<Assessment>,
    },
    Unchanged {
        accepted: AcceptedReference,
        source: ReportSource,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        assessments: Vec<Assessment>,
    },
    NeedsInput {
        reason: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Assessment {
    pub discussion_id: ResourceIdDto,
    pub conclusion: super::FindingConclusion,
    pub reason: String,
    pub finding_ids: Vec<ResourceIdDto>,
    pub next_action: crate::followups::NextAction,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReportSource {
    File {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha256: Option<ReportHashDto>,
    },
    Response,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportRevision {
    pub sha: Option<GitShaDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportVerdict {
    Approve,
    RequestChanges,
    Blocked,
    NotSpecified,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptedReference {
    pub receipt_id: ResourceIdDto,
    pub sha256: ReportHashDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedReport {
    pub receipt_id: ResourceIdDto,
    pub sha256: ReportHashDto,
    pub source: Option<ReportSource>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffState {
    PendingCompletion,
    Recorded,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffReceipt {
    pub receipt_id: ResourceIdDto,
    pub state: HandoffState,
    pub declaration_sha256: ReportHashDto,
}
