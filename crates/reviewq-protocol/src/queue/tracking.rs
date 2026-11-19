use serde::{Deserialize, Serialize};

use super::{
    GitShaDto, MergeRequestIidDto, Page, ProjectIdDto, ReviewIdDto, RevisionDto, Success,
    TicketKeyDto, UpstreamIdDto, UtcTimestampDto,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TrackReviewRequest {
    pub expected_revision: RevisionDto,
    pub url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TrackedReview {
    pub review_id: ReviewIdDto,
    pub project_id: ProjectIdDto,
    pub iid: MergeRequestIidDto,
    pub source_sha: GitShaDto,
}

pub type TrackReviewResponse = Success<TrackedReview>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TicketDetail {
    pub key: TicketKeyDto,
    pub title: String,
    pub url: String,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub assignee_id: Option<String>,
    pub description: Option<String>,
    pub estimate: Option<f64>,
    pub warnings: Vec<&'static str>,
    #[serde(flatten)]
    pub comments: Page<TicketComment>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TicketComment {
    pub id: UpstreamIdDto,
    pub author: Option<String>,
    pub author_id: Option<String>,
    pub body: String,
    pub created_at: UtcTimestampDto,
    pub updated_at: Option<UtcTimestampDto>,
}

pub type TicketDetailResponse = Success<TicketDetail>;
