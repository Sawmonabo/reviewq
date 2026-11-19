use reviewq_domain::identity::{MergeRequestIid, TicketKey};

use crate::access::{AppError, AppFuture};
use crate::workspace::ConfiguredProject;

use super::{MatchedReview, RefreshAttempt, ReviewSourceReader};

const HISTORY_PAGES: u32 = 2_000;
const OVERLAP_MS: i64 = 60_000;

pub struct ProjectHistoryRow {
    pub iid: MergeRequestIid,
    pub title: String,
    pub branch: String,
    pub updated_at_ms: i64,
    pub closed: bool,
    pub keys: Vec<TicketKey>,
}

pub struct ProjectHistoryPage {
    pub rows: Vec<ProjectHistoryRow>,
    pub more: bool,
}

pub struct HistoryCandidate {
    pub iid: MergeRequestIid,
    pub keys: Vec<TicketKey>,
}

pub trait ProjectHistoryStore: Send + Sync {
    fn begin_project_history(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
    ) -> AppFuture<'_, Option<i64>>;
    fn stage_project_history(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
        rows: Vec<ProjectHistoryRow>,
    ) -> AppFuture<'_, ()>;
    fn finish_project_history(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
        watermark: i64,
    ) -> AppFuture<'_, ()>;
    fn invalidate_project_history(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
    ) -> AppFuture<'_, ()>;
    fn project_history_candidates(
        &self,
        attempt: RefreshAttempt,
        project: ConfiguredProject,
        keys: Vec<TicketKey>,
    ) -> AppFuture<'_, Vec<HistoryCandidate>>;
}

pub(super) async fn closed_matches(
    store: &(impl ProjectHistoryStore + ?Sized),
    source: &dyn ReviewSourceReader,
    attempt: RefreshAttempt,
    project: ConfiguredProject,
    keys: Vec<TicketKey>,
) -> Result<Vec<MatchedReview>, AppError> {
    let watermark = store
        .begin_project_history(attempt, project.clone())
        .await?;
    let since = watermark.map(|time| time.saturating_sub(OVERLAP_MS).max(0));
    let result = collect(
        store,
        source,
        attempt,
        &project,
        since,
        watermark.unwrap_or(0),
    )
    .await;
    let next = match result {
        Ok(next) => next,
        Err(AppError::Conflict) if watermark.is_some() => {
            store.invalidate_project_history(attempt, project).await?;
            return Err(AppError::Conflict);
        }
        Err(error) => return Err(error),
    };
    store
        .finish_project_history(attempt, project.clone(), next)
        .await?;
    let candidates = store
        .project_history_candidates(attempt, project.clone(), keys)
        .await?;
    source.verify_history_candidates(project, candidates).await
}

async fn collect(
    store: &(impl ProjectHistoryStore + ?Sized),
    source: &dyn ReviewSourceReader,
    attempt: RefreshAttempt,
    project: &ConfiguredProject,
    since: Option<i64>,
    mut watermark: i64,
) -> Result<i64, AppError> {
    let mut previous = since.unwrap_or(i64::MIN);
    for page in 1..=HISTORY_PAGES {
        let observed = source
            .project_history_page(project.clone(), page, since)
            .await?;
        for row in &observed.rows {
            if row.updated_at_ms < previous {
                return Err(AppError::Conflict);
            }
            previous = row.updated_at_ms;
            watermark = watermark.max(previous);
        }
        if !observed.rows.is_empty() {
            store
                .stage_project_history(attempt, project.clone(), observed.rows)
                .await?;
        }
        if !observed.more {
            return Ok(watermark);
        }
    }
    Err(AppError::PayloadTooLarge)
}
