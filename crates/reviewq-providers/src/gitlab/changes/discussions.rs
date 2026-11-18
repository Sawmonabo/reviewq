use reviewq_app::access::AppError;
use reviewq_app::changes::{
    Discussion, DiscussionAnchor, DiscussionNote, PatchRevision, SourcePage,
};
use reviewq_domain::identity::MrIdentity;
use serde::Deserialize;

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
