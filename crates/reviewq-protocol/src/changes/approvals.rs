use serde::Serialize;

use crate::{GitShaDto, ReviewIdDto, SafeCountDto, Success, UpstreamIdDto, UtcTimestampDto};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApprovalView {
    pub review_id: ReviewIdDto,
    pub source_sha: GitShaDto,
    pub observed_at: UtcTimestampDto,
    pub personal_approval: Option<bool>,
    pub approvals: Option<ApprovalFacts>,
    pub rules: Option<Vec<ApprovalRule>>,
    pub warnings: Vec<ApprovalWarning>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ApprovalWarning {
    ApprovalsUnavailable,
    RulesUnavailable,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApprovalFacts {
    pub approved: Option<bool>,
    pub required: Option<SafeCountDto>,
    pub left: Option<SafeCountDto>,
    pub approvers: Vec<Approver>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Approver {
    pub id: UpstreamIdDto,
    pub username: Option<String>,
    pub approved_at: Option<UtcTimestampDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApprovalRule {
    pub id: UpstreamIdDto,
    pub name: String,
    pub approved: bool,
    pub required: SafeCountDto,
}

pub type ApprovalViewResponse = Success<ApprovalView>;
