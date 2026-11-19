use std::collections::{HashMap, HashSet};

use reviewq_domain::identity::{MrIdentity, ProjectId, TicketKey};
use reviewq_domain::settings::{BoardScope, BoardType, WorkspaceScope};

use crate::access::AppError;
use crate::queue::ReviewSource;
use crate::workspace::ConfiguredProject;

use super::{
    MAX_PUBLICATION_REVIEWS, RefreshAttempt, ReviewSourceReader, ScopedTicket, SourceRefreshStore,
    TicketSourceReader,
};

const MAX_PAGES: usize = 2_000;
const MAX_ISSUES: usize = 8_192;
const MAX_ASSOCIATIONS: usize = 16_384;
const MAX_SPRINTS: usize = 512;

pub struct MatchedReview {
    pub review: ReviewSource,
    pub keys: Vec<TicketKey>,
}

pub struct ScopedIssue {
    pub board: u64,
    pub ticket: ScopedTicket,
    pub sprint_ids: Vec<u64>,
}

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct ScopedMatch {
    pub board: u64,
    pub key: TicketKey,
}

pub struct ScopedSprint {
    pub board: u64,
    pub id: u64,
    pub label: String,
}

pub struct SprintPage {
    pub items: Vec<ScopedSprint>,
    pub next: Option<u32>,
}

#[derive(Default)]
pub struct SourceObservation {
    pub reviews: HashMap<MrIdentity, (ReviewSource, HashSet<ScopedMatch>)>,
    pub owned: Vec<ReviewSource>,
    pub issues: Vec<ScopedIssue>,
    pub sprints: Vec<ScopedSprint>,
}

impl SourceObservation {
    pub(super) fn validation(reviews: Vec<ReviewSource>) -> Self {
        Self {
            reviews: reviews
                .into_iter()
                .map(|review| (review.identity.clone(), (review, HashSet::new())))
                .collect(),
            ..Self::default()
        }
    }
}

/// Produces a complete compact scope before publishing any discovered row.
pub(super) async fn collect(
    store: &(impl SourceRefreshStore + ?Sized),
    gitlab: &dyn ReviewSourceReader,
    jira: &dyn TicketSourceReader,
    scope: &WorkspaceScope,
    projects: Vec<ConfiguredProject>,
    attempt: RefreshAttempt,
) -> Result<SourceObservation, AppError> {
    let mut observation = SourceObservation::default();
    let mut requirements = HashMap::<ProjectId, HashMap<TicketKey, Vec<u64>>>::new();
    let mut total_associations = 0;
    let mut total_memberships = 0;
    for board in scope.boards.iter().filter(|board| board.selection.enabled) {
        let board_id = board.selection.board_id;
        let mut issues = issues(jira, board, None).await?;
        if observation.issues.len() + issues.len() > MAX_ISSUES {
            return Err(AppError::PayloadTooLarge);
        }
        for project in scope.projects_for(board) {
            let keys = requirements.entry(project).or_default();
            for key in issues.keys() {
                total_associations += 1;
                if total_associations > MAX_ASSOCIATIONS {
                    return Err(AppError::PayloadTooLarge);
                }
                keys.entry(key.clone()).or_default().push(board_id);
            }
        }
        if board.board_type == BoardType::Scrum {
            let sprints = active_sprints(jira, board_id).await?;
            if observation.sprints.len() + sprints.len() > MAX_SPRINTS {
                return Err(AppError::PayloadTooLarge);
            }
            for sprint in sprints {
                for key in self::issues(jira, board, Some(sprint.id)).await?.keys() {
                    let issue = issues.get_mut(key).ok_or(AppError::Conflict)?;
                    total_memberships += 1;
                    if total_memberships > MAX_ASSOCIATIONS {
                        return Err(AppError::PayloadTooLarge);
                    }
                    issue.sprint_ids.push(sprint.id);
                }
                observation.sprints.push(sprint);
            }
        }
        observation.issues.extend(issues.into_values());
    }
    let mut open = HashSet::new();
    let mut matches = 0;
    for project in &projects {
        if let Some(keys) = requirements.get(&project.project_id) {
            if keys.is_empty() {
                continue;
            }
            for found in gitlab
                .ticket_reviews(project.clone(), keys.keys().cloned().collect(), true)
                .await?
            {
                let exact = matches_for(&found, keys);
                open.extend(
                    exact
                        .iter()
                        .map(|selection| (selection.board, selection.key.clone())),
                );
                add_match(&mut observation, found.review, exact, &mut matches)?;
            }
        }
    }
    for project in projects {
        if let Some(mut keys) = requirements.remove(&project.project_id) {
            keys.retain(|key, boards| {
                boards.retain(|board| !open.contains(&(*board, key.clone())));
                !boards.is_empty()
            });
            if keys.is_empty() {
                continue;
            }
            for found in gitlab
                .ticket_reviews(project.clone(), keys.keys().cloned().collect(), false)
                .await?
            {
                let exact = matches_for(&found, &keys);
                add_match(&mut observation, found.review, exact, &mut matches)?;
            }
        }
    }
    if !requirements.is_empty() {
        return Err(AppError::Conflict);
    }
    for identity in store.retained_reviews(attempt).await? {
        if !observation.reviews.contains_key(&identity) {
            match gitlab.review(identity).await {
                Ok(review) => observation.owned.push(review),
                Err(AppError::NotFound) => (),
                Err(error) => return Err(error),
            }
        }
    }
    Ok(observation)
}

async fn issues(
    jira: &dyn TicketSourceReader,
    board: &BoardScope,
    sprint: Option<u64>,
) -> Result<HashMap<TicketKey, ScopedIssue>, AppError> {
    let mut issues = HashMap::new();
    let mut seen = HashSet::new();
    let mut next = None;
    for _ in 0..MAX_PAGES {
        let page = jira.issues(board.clone(), sprint, next, 100).await?;
        if issues.len() + page.items.len() > MAX_ISSUES {
            return Err(AppError::PayloadTooLarge);
        }
        for ticket in page.items {
            let key = ticket.key.clone();
            let previous = issues.insert(
                key,
                ScopedIssue {
                    board: board.selection.board_id,
                    ticket,
                    sprint_ids: Vec::new(),
                },
            );
            if previous.is_some() {
                return Err(AppError::Conflict);
            }
        }
        let Some(token) = page.continuation else {
            return Ok(issues);
        };
        if !seen.insert(token.clone()) {
            return Err(AppError::Conflict);
        }
        next = Some(token);
    }
    Err(AppError::PayloadTooLarge)
}

async fn active_sprints(
    jira: &dyn TicketSourceReader,
    board: u64,
) -> Result<Vec<ScopedSprint>, AppError> {
    let mut sprints = Vec::new();
    let mut seen = HashSet::new();
    let mut offset = 0;
    for _ in 0..MAX_PAGES {
        let page = jira.sprints(board, offset).await?;
        if sprints.len() + page.items.len() > MAX_SPRINTS {
            return Err(AppError::PayloadTooLarge);
        }
        for sprint in page.items {
            if sprint.board != board || !seen.insert(sprint.id) {
                return Err(AppError::Conflict);
            }
            sprints.push(sprint);
        }
        let Some(next) = page.next else {
            return Ok(sprints);
        };
        if next <= offset {
            return Err(AppError::Conflict);
        }
        offset = next;
    }
    Err(AppError::PayloadTooLarge)
}

fn matches_for(
    review: &MatchedReview,
    keys: &HashMap<TicketKey, Vec<u64>>,
) -> HashSet<ScopedMatch> {
    let mut selections = HashSet::new();
    for key in &review.keys {
        if let Some(boards) = keys.get(key) {
            selections.extend(boards.iter().map(|board| ScopedMatch {
                board: *board,
                key: key.clone(),
            }));
        }
    }
    selections
}

fn add_match(
    observation: &mut SourceObservation,
    review: ReviewSource,
    selections: HashSet<ScopedMatch>,
    matches: &mut usize,
) -> Result<(), AppError> {
    if selections.is_empty() {
        return Ok(());
    }
    *matches += selections.len();
    if *matches > MAX_ASSOCIATIONS || observation.reviews.len() >= MAX_PUBLICATION_REVIEWS {
        return Err(AppError::PayloadTooLarge);
    }
    if observation
        .reviews
        .insert(review.identity.clone(), (review, selections))
        .is_some()
    {
        return Err(AppError::Conflict);
    }
    Ok(())
}
