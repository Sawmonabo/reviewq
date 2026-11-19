use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use reviewq_domain::identity::{ProjectId, ReviewId, Revision, TicketKey};
use reviewq_domain::settings::WorkspaceScope;

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::sources::TicketSourceReader;

use super::{PageContext, RunCard, SprintPair};

#[derive(Eq, PartialEq)]
pub struct ArchiveScope {
    pub settings: Revision,
    pub targets: Vec<ArchiveTarget>,
    pub limited: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveTarget {
    pub review: ReviewId,
    pub project: ProjectId,
    pub keys: BTreeSet<TicketKey>,
}

#[derive(Clone, Default)]
pub struct ArchiveMembership {
    pub pairs: BTreeSet<SprintPair>,
    pub complete: bool,
}

pub struct ArchiveBoard {
    pub id: u64,
    pub label: String,
    pub available: bool,
}

pub struct ArchiveMetadata {
    pub boards: Vec<ArchiveBoard>,
    pub membership: BTreeMap<ReviewId, ArchiveMembership>,
}

pub struct ArchiveFilter {
    pub search: String,
    pub from: Option<i64>,
    pub until: Option<i64>,
    pub pairs: Vec<SprintPair>,
}

pub struct ArchivePosition {
    pub context: PageContext,
    pub latest: Option<i64>,
    pub review: ReviewId,
}

pub struct ArchiveItem {
    pub card: RunCard,
    pub latest: Option<i64>,
    pub date: Option<ArchiveDate>,
    pub membership: ArchiveMembership,
}

pub enum ArchiveDate {
    Decision,
    Report,
    Followup,
}

pub struct ArchivePage {
    pub items: Vec<ArchiveItem>,
    pub next: Option<ArchivePosition>,
    pub total: u64,
    pub matching: u64,
}

pub trait ArchiveStore: Send + Sync {
    fn archive_scope(&self, actor: Actor) -> AppFuture<'_, Snapshot<ArchiveScope>>;
    fn read_archive(
        &self,
        actor: Actor,
        captured: Arc<ArchiveScope>,
        filter: ArchiveFilter,
        metadata: Arc<ArchiveMetadata>,
        after: Option<ArchivePosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ArchivePage>>;
}

pub async fn read_archive_metadata(
    scope: &WorkspaceScope,
    source: Option<&dyn TicketSourceReader>,
    targets: &[ArchiveTarget],
) -> Result<ArchiveMetadata, AppError> {
    let mut boards = Vec::new();
    let mut accumulated = BTreeMap::<ReviewId, (ArchiveMembership, bool, bool)>::new();
    for board in scope.boards.iter().filter(|b| b.selection.enabled) {
        let id = board.selection.board_id;
        let eligible = scope.projects_for(board).collect::<BTreeSet<_>>();
        let reviews = targets
            .iter()
            .filter(|target| eligible.contains(&target.project) && !target.keys.is_empty())
            .collect::<Vec<_>>();
        let keys = reviews
            .iter()
            .flat_map(|target| target.keys.iter().cloned())
            .collect::<BTreeSet<_>>();
        let observed = match source {
            Some(source) if !keys.is_empty() => match source
                .archive_memberships(board.clone(), keys.into_iter().collect())
                .await
            {
                Ok(found) => Some(found.into_iter().collect::<BTreeMap<_, _>>()),
                Err(error @ (AppError::StaleRevision | AppError::Busy)) => return Err(error),
                Err(_) => None,
            },
            Some(_) => Some(BTreeMap::new()),
            None => None,
        };
        boards.push(ArchiveBoard {
            id,
            label: board.label.clone(),
            available: observed.is_some(),
        });
        for target in reviews {
            let (membership, seen, partial) = accumulated.entry(target.review.clone()).or_default();
            if let Some(observed) = &observed {
                for item in target.keys.iter().filter_map(|key| observed.get(key)) {
                    *seen = true;
                    *partial |= !item.complete;
                    membership.pairs.extend(item.pairs.iter().copied());
                }
            } else {
                *partial = true;
            }
        }
    }
    let membership = accumulated
        .into_iter()
        .map(|(id, (mut membership, seen, partial))| {
            membership.complete = seen && !partial;
            (id, membership)
        })
        .collect();
    Ok(ArchiveMetadata { boards, membership })
}
