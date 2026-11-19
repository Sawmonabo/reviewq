use reviewq_app::access::AppError;
use reviewq_app::changes::{
    Discussion, DiscussionAnchor, DiscussionNote, PatchRevision, SourcePage,
};
use reviewq_app::sources::{
    ReviewSourceReader, WorkflowDiscussion, WorkflowNote, WorkflowSnapshot,
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
        .filter(|pos| pos.old_line.is_some() || pos.new_line.is_some())
        .filter(|pos| safe_path(&pos.old_path) && safe_path(&pos.new_path))
        .map(|pos| DiscussionAnchor {
            old_path: pos.old_path.clone(),
            new_path: pos.new_path.clone(),
            old_line: pos.old_line.filter(|line| *line != 0),
            new_line: pos.new_line.filter(|line| *line != 0),
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
                body: note.body,
                created_at_ms,
                resolved: note.resolved,
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

#[derive(Deserialize)]
struct ApprovalState {
    project_id: Option<u64>,
    iid: Option<u64>,
    approved_by: Vec<ApprovalActor>,
}

#[derive(Deserialize)]
struct ApprovalActor {
    user: ConnectedUser,
}

impl GitLab {
    pub(in crate::gitlab) async fn read_workflow_snapshot(
        &self,
        identity: MrIdentity,
    ) -> Result<WorkflowSnapshot, AppError> {
        let before = ReviewSourceReader::review(self, identity.clone()).await?;
        let (_, user) = self.get(self.url("api/v4/user")?, 32 * 1024).await?;
        let user: ConnectedUser =
            serde_json::from_slice(&user).map_err(|_| AppError::Unavailable)?;
        if user.id == 0 {
            return Err(AppError::Unavailable);
        }
        let mut discussions = Vec::new();
        let mut discussion_ids = std::collections::HashSet::new();
        let mut note_ids = std::collections::HashSet::new();
        let mut page = 1;
        loop {
            let (rows, next_page) = self
                .read_diff_rows::<UpstreamDiscussion>(
                    &identity,
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
        let url = self.url(&format!(
            "api/v4/projects/{}/merge_requests/{}/approvals",
            identity.project_id(),
            identity.iid()
        ))?;
        let (_, approvals) = self.get(url, MAX_DISCUSSION_BYTES).await?;
        let approvals: ApprovalState =
            serde_json::from_slice(&approvals).map_err(|_| AppError::Unavailable)?;
        if approvals
            .project_id
            .is_some_and(|id| id != identity.project_id().get())
            || approvals.iid.is_some_and(|iid| iid != identity.iid().get())
            || approvals.approved_by.iter().any(|actor| actor.user.id == 0)
        {
            return Err(AppError::Unavailable);
        }
        let after = ReviewSourceReader::review(self, identity.clone()).await?;
        if before != after || after.identity != identity {
            return Err(AppError::StaleRevision);
        }
        Ok(WorkflowSnapshot {
            review: after,
            authenticated_actor: user.id,
            personal_approval: approvals
                .approved_by
                .iter()
                .any(|actor| actor.user.id == user.id),
            observed_at_ms: jiff::Timestamp::now().as_millisecond(),
            discussions,
        })
    }
}
