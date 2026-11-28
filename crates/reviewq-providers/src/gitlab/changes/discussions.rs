use std::sync::Arc;
use std::time::Instant;

use reviewq_app::access::AppError;
use reviewq_app::changes::{
    Discussion, DiscussionAnchor, DiscussionLine, DiscussionNote, DiscussionPage,
    DiscussionPageRequest, DiscussionPosition, DiscussionRange, DiscussionResolution,
    PatchRevision, discussion_page_at,
};
use reviewq_app::sources::{
    DiscoveryEvidence, DiscussionNoteTime, ReviewSourceReader, WorkflowDiscussion, WorkflowNote,
    WorkflowSnapshot,
};
use reviewq_domain::identity::{MrIdentity, ReportHash};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{DiffRefs, GitLab, git_sha, revision_from_refs, safe_path};

mod cache;
mod drafts;
pub(in crate::gitlab) use cache::DiscussionCache;

const MAX_DISCUSSION_BYTES: usize = 512 * 1024;

#[derive(Deserialize)]
struct UpstreamDiscussion {
    id: String,
    notes: Vec<UpstreamNote>,
}

#[derive(Deserialize, Serialize)]
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
    resolvable: Option<bool>,
    resolved: Option<bool>,
    resolved_by: Option<ConnectedUser>,
    position: Option<Position>,
}

#[derive(Deserialize, Serialize)]
struct Author {
    #[serde(default)]
    id: Option<u64>,
    username: String,
}

#[derive(Deserialize, Serialize)]
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

#[derive(Deserialize, Serialize)]
struct LineRange {
    start: Line,
    end: Line,
}

#[derive(Deserialize, Serialize)]
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

impl Position {
    fn anchor(&self) -> Option<DiscussionAnchor> {
        if !safe_path(&self.old_path) || !safe_path(&self.new_path) {
            return None;
        }
        let (position_type, old_line, new_line, range) = match self.position_type.as_str() {
            "text" => {
                let range = self.line_range.as_ref().and_then(|range| {
                    Some(DiscussionRange {
                        start: range.start.project()?,
                        end: range.end.project()?,
                    })
                });
                let old_line = self
                    .old_line
                    .filter(|line| *line > 0)
                    .or_else(|| range.as_ref().and_then(|range| range.end.old_line));
                let new_line = self
                    .new_line
                    .filter(|line| *line > 0)
                    .or_else(|| range.as_ref().and_then(|range| range.end.new_line));
                (DiscussionPosition::Text, old_line, new_line, range)
            }
            "file" => (DiscussionPosition::File, None, None, None),
            _ => (DiscussionPosition::Other, None, None, None),
        };
        Some(DiscussionAnchor {
            base_sha: git_sha(&self.refs.base_sha).ok()?,
            start_sha: git_sha(&self.refs.start_sha).ok()?,
            head_sha: git_sha(&self.refs.head_sha).ok()?,
            position_type,
            old_path: self.old_path.clone(),
            new_path: self.new_path.clone(),
            old_line,
            new_line,
            range,
        })
    }
}

impl GitLab {
    pub(super) async fn discussion_page(
        &self,
        source: MrIdentity,
        request: DiscussionPageRequest,
    ) -> Result<DiscussionPage, AppError> {
        let DiscussionPageRequest {
            scope,
            page,
            limit,
            allow_partial,
        } = request;
        if page == 0 || page > 1000 || !(1..=50).contains(&limit) {
            return Err(AppError::InvalidRequest);
        }
        // One deadline for both waits for a place, so together they wait at most `LONGEST`.
        let deadline = reviewq_app::wait::deadline();
        let (current, mr_id) = self.discussion_context(&source, &scope.revision).await?;
        let (_place, observation) = self
            .discussions
            .observe(&source, mr_id, &scope, deadline)
            .await?;
        let (_page_place, slot) = observation.page(page, limit, deadline).await?;
        let cached = slot
            .get_or_try_init(|| async {
                let started = Instant::now();
                let raw = self
                    .read_diff_rows::<UpstreamDiscussion>(
                        &source,
                        "discussions",
                        page,
                        limit,
                        MAX_DISCUSSION_BYTES,
                    )
                    .await;
                let (rows, next) = match raw {
                    Err(AppError::PayloadTooLarge) => return Ok(None),
                    result => result?,
                };
                let mut items = Vec::with_capacity(rows.len());
                let mut fingerprint = Vec::with_capacity(rows.len());
                for row in rows {
                    let normalized = project(row, &source, &current)?;
                    fingerprint.push(normalized.as_ref().map(|item| item.fingerprint));
                    if let Some(item) = normalized {
                        items.push(item);
                    }
                }
                self.confirm_discussion_context(&source, &current.id, mr_id)
                    .await?;
                let weight = items.iter().map(cache::discussion_bytes).sum::<usize>()
                    + fingerprint.capacity() * size_of::<Option<[u8; 32]>>();
                let limited = items.iter().any(|thread| thread.limited);
                items.retain(|thread| !thread.notes.is_empty());
                Ok::<_, AppError>(Some(cache::Page {
                    items: Arc::from(items),
                    next,
                    weight,
                    limited,
                    fingerprint,
                    started,
                }))
            })
            .await?;
        self.discussions.check(&source, &scope, &observation)?;
        observation.trim()?;
        if cached.as_ref().map_or(!allow_partial || limit > 1, |page| {
            !allow_partial && page.limited
        }) {
            return Err(AppError::PayloadTooLarge);
        }
        Ok(DiscussionPage {
            revision: current,
            observation: observation.id.clone(),
            items: cached
                .as_ref()
                .map_or_else(|| Arc::from([]), |page| page.items.clone()),
            next_page: cached
                .as_ref()
                .map_or_else(|| (page < 1000).then_some(page + 1), |page| page.next),
            limited: cached.as_ref().is_none_or(|page| page.limited),
        })
    }

    async fn discussion_context(
        &self,
        source: &MrIdentity,
        revision: &str,
    ) -> Result<(PatchRevision, Option<u64>), AppError> {
        if source.origin() != &self.identity {
            return Err(AppError::NotFound);
        }
        let row = self
            .merge_request(source.project_id(), source.iid())
            .await?;
        let head = row.head_sha()?;
        let refs = row.diff_refs.ok_or(AppError::Unavailable)?;
        let revision = revision_from_refs(source, refs, head, Some(revision))?;
        Ok((revision, row.id))
    }

    async fn confirm_discussion_context(
        &self,
        source: &MrIdentity,
        revision: &str,
        expected_id: Option<u64>,
    ) -> Result<(), AppError> {
        let (_, id) = self.discussion_context(source, revision).await?;
        if id != expected_id {
            return Err(AppError::StaleRevision);
        }
        Ok(())
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
    let Some(fingerprint) = row.fingerprint()? else {
        return Ok(None);
    };
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
    let anchor = position.and_then(Position::anchor);
    let mut limited = false;
    let notes = std::iter::once(first)
        .chain(notes)
        .filter_map(|note| {
            if note.id == 0
                || note.project_id != source.project_id().get()
                || note
                    .noteable_iid
                    .is_some_and(|iid| iid != source.iid().get())
                || note.author.username.len() > 256
            {
                return Some(Err(AppError::Unavailable));
            }
            if note.body.len() > 64 * 1024 {
                limited = true;
                return None;
            }
            let created_at_ms = note
                .created_at
                .parse::<jiff::Timestamp>()
                .ok()
                .map(|stamp| stamp.as_millisecond());
            Some(Ok(DiscussionNote {
                id: note.id,
                author: note.author.username,
                author_id: note.author.id,
                body: note.body,
                created_at_ms,
                resolvable: note.resolvable,
                resolved: note.resolved,
                resolved_by: note.resolved_by.map(|user| user.id),
            }))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Some(Discussion {
        id: row.id,
        fingerprint,
        limited,
        anchor,
        outdated,
        notes,
    }))
}

impl UpstreamDiscussion {
    fn fingerprint(&self) -> Result<Option<[u8; 32]>, AppError> {
        let visible = self
            .notes
            .iter()
            .filter(|note| !note.system && !note.confidential)
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return Ok(None);
        }
        Ok(Some(content_fingerprint(&(&self.id, visible))?))
    }
}

fn content_fingerprint(value: &impl Serialize) -> Result<[u8; 32], AppError> {
    struct Sink(Sha256);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink(Sha256::new());
    serde_json::to_writer(&mut sink, value).map_err(|_| AppError::Internal)?;
    Ok(sink.0.finalize().into())
}

const MAX_WORKFLOW_DISCUSSIONS: usize = 2_000;
const MAX_WORKFLOW_NOTES: usize = 5_000;

#[derive(Deserialize, Serialize)]
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
        let (discussions, discussion_fingerprint) = match discussions {
            Ok((discussions, fingerprint)) => (
                Some(
                    discussions
                        .into_iter()
                        .map(|thread| reviewq_app::sources::DiscoveryDiscussion {
                            resolution: thread.resolution,
                            notes: thread.notes.iter().map(DiscussionNoteTime::from).collect(),
                        })
                        .collect(),
                ),
                Some(fingerprint),
            ),
            Err(AppError::StaleRevision) => return Err(AppError::StaleRevision),
            Err(_) => (None, None),
        };
        let review = ReviewSourceReader::review(self, identity).await?;
        if before != review {
            return Err(AppError::StaleRevision);
        }
        Ok(DiscoveryEvidence {
            review,
            authenticated_actor,
            approved_reviewers,
            discussions,
            discussion_fingerprint,
            observed_at_ms: jiff::Timestamp::now().as_millisecond(),
        })
    }

    async fn workflow_discussions(
        &self,
        identity: &MrIdentity,
    ) -> Result<(Vec<WorkflowDiscussion>, [u8; 32]), AppError> {
        let started = Instant::now();
        let mut discussions = Vec::new();
        let mut fingerprints = Vec::new();
        let mut discussion_ids = std::collections::HashSet::new();
        let mut note_ids = std::collections::HashSet::new();
        let (mut page, mut limit) = (1, 40);
        loop {
            let result = self
                .read_diff_rows::<UpstreamDiscussion>(
                    identity,
                    "discussions",
                    page,
                    limit,
                    MAX_DISCUSSION_BYTES,
                )
                .await;
            let (rows, next_page) = match result {
                Err(AppError::PayloadTooLarge) if limit > 1 => {
                    (page, limit) = discussion_page_at((page - 1) * u32::from(limit), limit - 1);
                    continue;
                }
                result => result?,
            };
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
                fingerprints.push(discussion.fingerprint()?);
                let root_note_id = discussion
                    .notes
                    .first()
                    .filter(|note| !note.system && !note.confidential)
                    .map(|note| note.id);
                let visible = || {
                    discussion
                        .notes
                        .iter()
                        .filter(|note| !note.system && !note.confidential)
                };
                let resolution = DiscussionResolution::from_notes(
                    visible().map(|note| (note.resolvable, note.resolved)),
                );
                let resolved_by = resolution.resolved().and_then(|resolved| {
                    let mut actors = visible()
                        .filter(|note| {
                            note.resolvable == Some(true) && note.resolved == Some(resolved)
                        })
                        .map(|note| {
                            note.resolved_by
                                .as_ref()
                                .map(|user| user.id)
                                .filter(|id| *id > 0)
                        });
                    let actor = actors.next().flatten()?;
                    actors
                        .all(|observed| observed == Some(actor))
                        .then_some(actor)
                });
                let mut notes = Vec::new();
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
                if !notes.is_empty() {
                    discussions.push(WorkflowDiscussion {
                        id: discussion.id,
                        root_note_id,
                        resolved_by,
                        resolution,
                        notes,
                    });
                }
            }
            let Some(next) = next_page else { break };
            let offset = (next - 1) * u32::from(limit);
            if offset as usize >= MAX_WORKFLOW_DISCUSSIONS {
                return Err(AppError::PayloadTooLarge);
            }
            (page, limit) = discussion_page_at(offset, 40);
        }
        self.discussions
            .invalidate_if_changed(identity, started, &fingerprints)?;
        let mut fingerprint = Sha256::new();
        for row in fingerprints {
            if let Some(hash) = row {
                fingerprint.update([1]);
                fingerprint.update(hash);
            } else {
                fingerprint.update([0]);
            }
        }
        Ok((discussions, fingerprint.finalize().into()))
    }

    pub(in crate::gitlab) async fn read_workflow_snapshot(
        &self,
        identity: MrIdentity,
    ) -> Result<WorkflowSnapshot, AppError> {
        let before = ReviewSourceReader::review(self, identity.clone()).await?;
        let user = self.approval_account().await?;
        let (discussions, discussion_fingerprint) = self.workflow_discussions(&identity).await?;
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
            discussion_fingerprint,
        })
    }
}
