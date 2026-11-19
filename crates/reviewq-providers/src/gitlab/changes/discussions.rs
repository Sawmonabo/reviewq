use reviewq_app::access::AppError;
use reviewq_app::changes::{
    Discussion, DiscussionAnchor, DiscussionLine, DiscussionNote, DiscussionRange, PatchRevision,
    SourcePage,
};
use reviewq_app::sources::{
    DiscoveryEvidence, ReviewSourceReader, WorkflowDiscussion, WorkflowNote, WorkflowSnapshot,
};
use reviewq_domain::identity::{MrIdentity, ReportHash};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{DiffRefs, GitLab, safe_path};

const MAX_DISCUSSION_BYTES: usize = 512 * 1024;

#[derive(Deserialize)]
struct UpstreamDiscussion {
    id: String,
    notes: Vec<UpstreamNote>,
}

#[derive(Deserialize)]
struct UpstreamNote {
    id: u64,
    body: String,
    author: Author,
    created_at: String,
    project_id: u64,
    noteable_iid: Option<u64>,
    #[serde(default)]
    confidential: bool,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    resolved: bool,
    resolved_by: Option<ConnectedUser>,
    position: Option<Position>,
}

#[derive(Deserialize)]
struct Author {
    #[serde(default)]
    id: Option<u64>,
    username: String,
}

#[derive(Deserialize)]
struct Position {
    #[serde(flatten)]
    refs: DiffRefs,
    position_type: String,
    old_path: String,
    new_path: String,
    old_line: Option<u32>,
    new_line: Option<u32>,
    line_range: Option<LineRange>,
}

#[derive(Deserialize)]
struct LineRange {
    start: Line,
    end: Line,
}

#[derive(Deserialize)]
struct Line {
    old_line: Option<u32>,
    new_line: Option<u32>,
}

impl Line {
    fn project(&self) -> Option<DiscussionLine> {
        let old_line = self.old_line.filter(|line| *line > 0);
        let new_line = self.new_line.filter(|line| *line > 0);
        (old_line.is_some() || new_line.is_some()).then_some(DiscussionLine { old_line, new_line })
    }
}

impl GitLab {
    pub(super) async fn discussion_page(
        &self,
        source: MrIdentity,
        revision: String,
        page: u32,
        limit: u16,
    ) -> Result<SourcePage<Discussion>, AppError> {
        if page == 0 || page > 1000 || !(1..=50).contains(&limit) {
            return Err(AppError::InvalidRequest);
        }
        let current = self.patch_revision(&source, Some(&revision)).await?;
        let (rows, next_page) = self
            .read_diff_rows::<UpstreamDiscussion>(
                &source,
                "discussions",
                page,
                limit,
                MAX_DISCUSSION_BYTES,
            )
            .await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(item) = project(row, &source, &current)? {
                items.push(item);
            }
        }
        self.patch_revision(&source, Some(&current.id)).await?;
        Ok(SourcePage {
            revision: current,
            items,
            next_page,
        })
    }
}

fn project(
    row: UpstreamDiscussion,
    source: &MrIdentity,
    revision: &PatchRevision,
) -> Result<Option<Discussion>, AppError> {
    if row.id.is_empty() || row.id.len() > 256 || row.id.chars().any(char::is_control) {
        return Err(AppError::Unavailable);
    }
    let mut notes = row
        .notes
        .into_iter()
        .filter(|note| !note.system && !note.confidential);
    let Some(first) = notes.next() else {
        return Ok(None);
    };
    let position = first.position.as_ref();
    let outdated = position.is_some_and(|pos| {
        pos.refs.head_sha != revision.head.as_str()
            || pos.refs.base_sha != revision.base.as_str()
            || pos.refs.start_sha != revision.start.as_str()
    });
    let anchor = position
        .filter(|pos| !outdated && pos.position_type == "text")
        .filter(|pos| safe_path(&pos.old_path) && safe_path(&pos.new_path))
        .and_then(|pos| {
            let range = pos.line_range.as_ref().and_then(|range| {
                Some(DiscussionRange {
                    start: range.start.project()?,
                    end: range.end.project()?,
                })
            });
            let old_line = pos
                .old_line
                .filter(|line| *line > 0)
                .or_else(|| range.as_ref().and_then(|range| range.end.old_line));
            let new_line = pos
                .new_line
                .filter(|line| *line > 0)
                .or_else(|| range.as_ref().and_then(|range| range.end.new_line));
            (old_line.is_some() || new_line.is_some()).then(|| DiscussionAnchor {
                old_path: pos.old_path.clone(),
                new_path: pos.new_path.clone(),
                old_line,
                new_line,
                range,
            })
        });
    let notes = std::iter::once(first)
        .chain(notes)
        .map(|note| {
            if note.id == 0
                || note.project_id != source.project_id().get()
                || note
                    .noteable_iid
                    .is_some_and(|iid| iid != source.iid().get())
                || note.body.len() > 64 * 1024
                || note.author.username.len() > 256
            {
                return Err(AppError::Unavailable);
            }
            let created_at_ms = note
                .created_at
                .parse::<jiff::Timestamp>()
                .ok()
                .map(|stamp| stamp.as_millisecond());
            Ok(DiscussionNote {
                id: note.id,
                author: note.author.username,
                author_id: note.author.id,
                body: note.body,
                created_at_ms,
                resolved: note.resolved,
                resolved_by: note.resolved_by.map(|user| user.id),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Some(Discussion {
        id: row.id,
        anchor,
        outdated,
        notes,
    }))
}

const MAX_WORKFLOW_DISCUSSIONS: usize = 2_000;
const MAX_WORKFLOW_NOTES: usize = 5_000;

#[derive(Deserialize)]
struct ConnectedUser {
    id: u64,
}

impl GitLab {
    pub(in crate::gitlab) async fn read_discovery_evidence(
        &self,
        identity: MrIdentity,
    ) -> Result<DiscoveryEvidence, AppError> {
        let before = ReviewSourceReader::review(self, identity.clone()).await?;
        let authenticated_actor = self.approval_account().await?;
        let (facts, discussions) = tokio::join!(
            self.read_approval_facts(&identity),
            self.workflow_discussions(&identity)
        );
        let approved_reviewers =
            facts?.map(|facts| facts.approvers.into_iter().map(|value| value.id).collect());
        let note_authors = match discussions {
            Ok(discussions) => Some(
                discussions
                    .into_iter()
                    .flat_map(|thread| thread.notes)
                    .map(|note| note.author)
                    .collect(),
            ),
            Err(AppError::StaleRevision) => return Err(AppError::StaleRevision),
            Err(_) => None,
        };
        let review = ReviewSourceReader::review(self, identity).await?;
        if before != review {
            return Err(AppError::StaleRevision);
        }
        Ok(DiscoveryEvidence {
            review,
            authenticated_actor,
            approved_reviewers,
            note_authors,
            observed_at_ms: jiff::Timestamp::now().as_millisecond(),
        })
    }

    async fn workflow_discussions(
        &self,
        identity: &MrIdentity,
    ) -> Result<Vec<WorkflowDiscussion>, AppError> {
        let mut discussions = Vec::new();
        let mut discussion_ids = std::collections::HashSet::new();
        let mut note_ids = std::collections::HashSet::new();
        let mut page = 1;
        loop {
            let (rows, next_page) = self
                .read_diff_rows::<UpstreamDiscussion>(
                    identity,
                    "discussions",
                    page,
                    30,
                    MAX_DISCUSSION_BYTES,
                )
                .await?;
            for discussion in rows {
                if discussion.id.is_empty()
                    || discussion.id.len() > 256
                    || discussion.id.chars().any(char::is_control)
                    || !discussion_ids.insert(discussion.id.clone())
                {
                    return Err(AppError::Unavailable);
                }
                if discussion_ids.len() > MAX_WORKFLOW_DISCUSSIONS {
                    return Err(AppError::PayloadTooLarge);
                }
                let root_note_id = discussion
                    .notes
                    .first()
                    .filter(|note| !note.system && !note.confidential)
                    .map(|note| note.id);
                let resolved_by = discussion
                    .notes
                    .first()
                    .and_then(|note| note.resolved_by.as_ref())
                    .map(|user| user.id)
                    .filter(|id| *id > 0);
                let mut notes = Vec::new();
                let mut resolved = None;
                for note in discussion.notes {
                    if note.system || note.confidential {
                        continue;
                    }
                    let actor = note
                        .author
                        .id
                        .filter(|id| *id > 0)
                        .ok_or(AppError::Unavailable)?;
                    let created = note
                        .created_at
                        .parse::<jiff::Timestamp>()
                        .map_err(|_| AppError::Unavailable)?;
                    if note.id == 0
                        || note.project_id != identity.project_id().get()
                        || note
                            .noteable_iid
                            .is_some_and(|iid| iid != identity.iid().get())
                        || !note_ids.insert(note.id)
                    {
                        return Err(AppError::Unavailable);
                    }
                    if note_ids.len() > MAX_WORKFLOW_NOTES {
                        return Err(AppError::PayloadTooLarge);
                    }
                    resolved.get_or_insert(note.resolved);
                    let positioned_head = note
                        .position
                        .map(|position| position.refs.head_sha.parse())
                        .transpose()
                        .map_err(|_| AppError::Unavailable)?;
                    notes.push(WorkflowNote {
                        id: note.id,
                        author: actor,
                        body_hash: ReportHash::from_sha256(
                            Sha256::digest(note.body.as_bytes()).into(),
                        ),
                        positioned_head,
                        created_at_ms: created.as_millisecond(),
                    });
                }
                if let Some(resolved) = resolved {
                    discussions.push(WorkflowDiscussion {
                        id: discussion.id,
                        root_note_id,
                        resolved_by,
                        resolved,
                        notes,
                    });
                }
            }
            let Some(next) = next_page else { break };
            if next > 100 {
                return Err(AppError::PayloadTooLarge);
            }
            page = next;
        }
        Ok(discussions)
    }

    pub(in crate::gitlab) async fn read_workflow_snapshot(
        &self,
        identity: MrIdentity,
    ) -> Result<WorkflowSnapshot, AppError> {
        let before = ReviewSourceReader::review(self, identity.clone()).await?;
        let user = self.approval_account().await?;
        let discussions = self.workflow_discussions(&identity).await?;
        let approved_reviewers = self.read_approval_facts(&identity).await?.map(|facts| {
            facts
                .approvers
                .into_iter()
                .map(|actor| actor.id)
                .collect::<Vec<_>>()
        });
        let after = ReviewSourceReader::review(self, identity.clone()).await?;
        if before != after || after.identity != identity {
            return Err(AppError::StaleRevision);
        }
        Ok(WorkflowSnapshot {
            review: after,
            authenticated_actor: user,
            personal_approval: approved_reviewers
                .as_ref()
                .map(|actors| actors.contains(&user)),
            approved_reviewers,
            observed_at_ms: jiff::Timestamp::now().as_millisecond(),
            discussions,
        })
    }
}
