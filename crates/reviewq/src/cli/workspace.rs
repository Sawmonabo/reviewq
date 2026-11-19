use std::path::Path;

use clap::ValueEnum;
use reviewq_domain::identity::{GitSha, NotificationId, SubmissionId};
use reviewq_protocol::decisions as dto;
use reviewq_protocol::{GitShaDto, SubmissionIdDto};

use crate::{native_client, runtime};

use super::Failure;

#[derive(Clone, Copy, ValueEnum)]
#[value(rename_all = "snake_case")]
pub(super) enum Disposition {
    Reviewed,
    WaitingAuthor,
    NeedsReview,
}

impl From<Disposition> for dto::Disposition {
    fn from(value: Disposition) -> Self {
        match value {
            Disposition::Reviewed => Self::Reviewed,
            Disposition::WaitingAuthor => Self::WaitingAuthor,
            Disposition::NeedsReview => Self::NeedsReview,
        }
    }
}

pub(super) async fn review(
    profile: &Path,
    target: String,
    sha: GitSha,
    disposition: Disposition,
    submission: Option<SubmissionId>,
) -> Result<(), Failure> {
    let submission = match submission {
        Some(value) => value,
        None => uuid::Uuid::new_v4().to_string().parse()?,
    };
    let body = serde_json::to_vec(&dto::TerminalDecision {
        target,
        source_sha: GitShaDto::new(sha.as_str()).ok_or("Invalid Git SHA.")?,
        disposition: disposition.into(),
        submission_id: SubmissionIdDto::new(submission.as_str()).ok_or("Invalid submission ID.")?,
    })?;
    request(profile, "/api/v1/control/review", &body, 8192)
        .await
        .map_err(|error| format!("{error}. Decision submission ID {submission}.").into())
}

pub(super) async fn ack(profile: &Path, ids: Vec<NotificationId>) -> Result<(), Failure> {
    let ids: Vec<_> = ids.iter().map(NotificationId::as_str).collect();
    let body = serde_json::to_vec(&serde_json::json!({"ids":ids}))?;
    request(profile, "/api/v1/control/ack", &body, 160 * 1024).await
}

async fn request(profile: &Path, path: &str, body: &[u8], limit: usize) -> Result<(), Failure> {
    let endpoint = runtime::read_endpoint(profile)?;
    let client = native_client::client()?;
    let status = native_client::signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    native_client::verify_ready(&endpoint, &status)?;
    let response =
        native_client::signed_limit(&client, &endpoint, "POST", path, body, limit).await?;
    println!("{}", String::from_utf8(response)?);
    Ok(())
}
