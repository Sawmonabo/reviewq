use std::collections::HashSet;

use reviewq_domain::identity::Revision;
use reviewq_domain::settings::{DefaultSkills, Provider, WorkspaceDraft, WorkspaceScope};

use crate::access::AppError;

use super::{ConnectionSources, DraftConnection, SavedCredentials, TokenChange};

/// A subset of the already verified workspace. Only the local proof builds this value.
pub struct ReducedWorkspace(WorkspaceScope);

impl ReducedWorkspace {
    pub fn into_scope(self) -> WorkspaceScope {
        self.0
    }
}

pub fn local_reduction(
    sources: &dyn ConnectionSources,
    current: &SavedCredentials,
    expected: Revision,
    gitlab: &DraftConnection,
    jira: &DraftConnection,
    draft: &WorkspaceDraft,
) -> Result<Option<ReducedWorkspace>, AppError> {
    if current.revision != expected {
        return Err(AppError::StaleRevision);
    }
    for (provider, input) in [(Provider::GitLab, gitlab), (Provider::Jira, jira)] {
        if !matches!(input.token, TokenChange::Keep) {
            return Ok(None);
        }
        let normalized = sources.normalize(input.connection.clone())?;
        if current
            .connection(provider)
            .is_none_or(|saved| saved.account.connection != normalized)
        {
            return Ok(None);
        }
    }
    Ok(subset(&current.scope, draft))
}

fn subset(current: &WorkspaceScope, draft: &WorkspaceDraft) -> Option<ReducedWorkspace> {
    let mut reviewers = draft.additional_reviewers.clone();
    let mut existing_reviewers = current.additional_reviewers.clone();
    reviewers.sort_unstable();
    existing_reviewers.sort_unstable();
    if reviewers != existing_reviewers
        || draft.repositories.len() > current.repositories.len()
        || draft.boards.len() > current.boards.len()
    {
        return None;
    }

    let mut changed = draft.repositories.len() < current.repositories.len()
        || draft.boards.len() < current.boards.len();
    let mut projects = HashSet::new();
    let mut result = WorkspaceScope {
        additional_reviewers: reviewers,
        ..WorkspaceScope::default()
    };
    for selection in &draft.repositories {
        let mut existing = current
            .repositories
            .iter()
            .find(|repo| repo.project == selection.project)?
            .clone();
        if !projects.insert(selection.project)
            || selection.board_selection != existing.board_selection
        {
            return None;
        }
        match (&selection.checkout, &existing.checkout) {
            (Some(path), Some(binding)) if *path == binding.root => {
                if selection.default_skills != existing.default_skills {
                    return None;
                }
            }
            (None, prior) if selection.default_skills == DefaultSkills::default() => {
                changed |= prior.is_some();
                existing.checkout = None;
                existing.default_skills = DefaultSkills::default();
            }
            _ => return None,
        }
        result.repositories.push(existing);
    }

    let mut seen = HashSet::new();
    for selection in &draft.boards {
        let mut existing = current
            .boards
            .iter()
            .find(|board| board.selection.board_id == selection.board_id)?
            .clone();
        if !seen.insert(selection.board_id) || (selection.enabled && !existing.selection.enabled) {
            return None;
        }
        let selected: HashSet<_> = selection.projects.iter().copied().collect();
        if selected.len() != selection.projects.len()
            || selected.iter().any(|project| {
                !projects.contains(project) || !existing.selection.projects.contains(project)
            })
        {
            return None;
        }
        changed |= existing.selection.enabled != selection.enabled
            || selected.len() < existing.selection.projects.len();
        let mut asserted = selection.clone();
        existing.selection.enabled = asserted.enabled;
        existing
            .selection
            .projects
            .retain(|project| selected.contains(project));
        existing.selection.projects.sort_unstable();
        asserted.projects.sort_unstable();
        if existing.selection != asserted {
            return None;
        }
        if existing.selection.enabled && result.projects_for(&existing).next().is_none() {
            existing.selection.enabled = false;
        }
        result.boards.push(existing);
    }
    changed.then_some(ReducedWorkspace(result))
}
