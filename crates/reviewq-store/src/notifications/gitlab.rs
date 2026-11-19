use std::collections::BTreeSet;

use reviewq_app::access::AppError;
use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_app::sources::{WorkflowDiscussion, WorkflowSnapshot};
use reviewq_domain::identity::{GitSha, NotificationId, ReviewId};
use rusqlite::{Connection, OptionalExtension, params};

use super::insert_notification;
use crate::StoreError;
use crate::queue::parse;

pub(crate) fn observe_head(
    db: &Connection,
    review: &ReviewId,
    head: &GitSha,
    updated_at: i64,
) -> Result<Option<NotificationId>, StoreError> {
    let prior: Option<(String, i64, i64)> = db
        .query_row(
            "SELECT sha,updated_at_ms,version FROM notification_gitlab_heads WHERE review_id=?1",
            [review.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((previous, last, version)) = prior else {
        db.execute(
            "INSERT INTO notification_gitlab_heads VALUES(?1,?2,?3,1)",
            params![review.as_str(), head.as_str(), updated_at],
        )?;
        return Ok(None);
    };
    if updated_at < last || (updated_at == last && previous == head.as_str()) {
        return Ok(None);
    }
    let changed = previous != head.as_str();
    let version = next_version(version, changed)?;
    db.execute(
        "UPDATE notification_gitlab_heads SET sha=?2,updated_at_ms=?3,version=?4 WHERE review_id=?1",
        params![review.as_str(),head.as_str(),updated_at,version],
    )?;
    if !changed {
        return Ok(None);
    }
    let reviewed: bool = db.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM review_decisions WHERE review_id=?1 AND reviewed_sha IS NOT NULL AND reviewed_sha<>?2
           UNION ALL SELECT 1 FROM runs r JOIN report_heads h USING(run_id)
             JOIN report_versions v USING(run_id,receipt_id) JOIN artifacts a USING(artifact_id)
             WHERE r.review_id=?1 AND v.full_mr_reviewed_sha<>?2 AND a.sealed=1 AND a.complete=1
           UNION ALL SELECT 1 FROM notification_gitlab_observations WHERE review_id=?1 AND personally_approved=1
         )", params![review.as_str(),head.as_str()], |row| row.get(0),
    )?;
    if !reviewed {
        return Ok(None);
    }
    observe(
        db,
        review,
        Kind::NewHead,
        "head",
        Some(head.to_string()),
        version,
        updated_at,
    )
}

pub(crate) fn collect(
    db: &Connection,
    review: &ReviewId,
    snapshot: &WorkflowSnapshot,
    reviewers: &BTreeSet<u64>,
) -> Result<Vec<NotificationId>, StoreError> {
    let mut events = Vec::new();
    if let Some(id) = observe_head(
        db,
        review,
        &snapshot.review.head_sha,
        snapshot.review.updated_at_ms,
    )? {
        events.push(id);
    }
    let prior: Option<(String, i64, Option<bool>, i64)> = db.query_row(
        "SELECT actor_id,observed_at_ms,personally_approved,approval_version FROM notification_gitlab_observations WHERE review_id=?1",
        [review.as_str()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).optional()?;
    if prior
        .as_ref()
        .is_some_and(|(_, last, _, _)| *last >= snapshot.observed_at_ms)
    {
        return Ok(events);
    }
    let account = snapshot.authenticated_actor.to_string();
    let last = prior.as_ref().filter(|(actor, _, _, _)| actor == &account);
    let prior_approval = last.and_then(|(_, _, approval, _)| *approval);
    let approved = snapshot.personal_approval.or(prior_approval);
    let version = next_version(
        prior.as_ref().map_or(0, |(_, _, _, version)| *version),
        last.is_none() || prior_approval != approved,
    )?;
    if prior_approval == Some(true)
        && approved == Some(false)
        && let Some(id) = observe(
            db,
            review,
            Kind::ApprovalWithdrawn,
            "approval",
            None,
            version,
            snapshot.observed_at_ms,
        )?
    {
        events.push(id);
    }
    let mut compare = Compare {
        db,
        review,
        reviewers,
        observed: snapshot.observed_at_ms,
        last: last.map(|(_, time, _, _)| *time),
        events: &mut events,
    };
    for discussion in &snapshot.discussions {
        compare.discussion(discussion)?;
    }
    db.execute(
        "INSERT INTO notification_gitlab_observations VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(review_id) DO UPDATE SET actor_id=excluded.actor_id,
           observed_at_ms=excluded.observed_at_ms,personally_approved=excluded.personally_approved,
           approval_version=excluded.approval_version",
        params![
            review.as_str(),
            account,
            snapshot.observed_at_ms,
            approved,
            version
        ],
    )?;
    Ok(events)
}

struct Compare<'a> {
    db: &'a Connection,
    review: &'a ReviewId,
    reviewers: &'a BTreeSet<u64>,
    observed: i64,
    last: Option<i64>,
    events: &'a mut Vec<NotificationId>,
}

impl Compare<'_> {
    fn discussion(&mut self, discussion: &WorkflowDiscussion) -> Result<(), StoreError> {
        let Some(first_owned) = discussion
            .notes
            .iter()
            .position(|note| self.reviewers.contains(&note.author))
        else {
            return Ok(());
        };
        let Some(root) = discussion.root_note_id.map(|id| id.to_string()) else {
            return Ok(());
        };
        let prior: Option<(bool, i64)> = self.db.query_row(
            "SELECT resolved,version FROM notification_gitlab_threads WHERE review_id=?1 AND root_note_id=?2",
            params![self.review.as_str(),root], |row| Ok((row.get(0)?,row.get(1)?)),
        ).optional()?;
        self.resolution(discussion, &root, prior)?;
        let mut lookup = self.db.prepare_cached(
            "SELECT actor_id,sha256,version FROM notification_gitlab_notes WHERE review_id=?1 AND note_id=?2"
        )?;
        let mut upsert = self.db.prepare_cached(
            "INSERT INTO notification_gitlab_notes VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(review_id,note_id) DO UPDATE SET actor_id=excluded.actor_id,sha256=excluded.sha256,version=excluded.version",
        )?;
        for note in discussion.notes.iter().skip(first_owned + 1) {
            if self.reviewers.contains(&note.author) {
                continue;
            }
            let id = note.id.to_string();
            let author = note.author.to_string();
            let hash = note.body_hash.as_str();
            let previous: Option<(String, String, i64)> = lookup
                .query_row(params![self.review.as_str(), id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .optional()?;
            if previous
                .as_ref()
                .is_some_and(|(actor, body, _)| actor == &author && body == hash)
            {
                continue;
            }
            let version = next_version(previous.as_ref().map_or(0, |(_, _, v)| *v), true)?;
            upsert.execute(params![self.review.as_str(), id, author, hash, version])?;
            let kind = match previous {
                Some((actor, body, _))
                    if actor == author && body != hash && self.last.is_some() =>
                {
                    Some(Kind::EditedReply)
                }
                None if self
                    .last
                    .is_some_and(|previous| prior.is_some() || note.created_at_ms > previous) =>
                {
                    Some(Kind::Reply)
                }
                _ => None,
            };
            if let Some(kind) = kind {
                self.event(
                    discussion,
                    kind,
                    &format!("note-{id}"),
                    Some(&id),
                    Some(hash),
                    version,
                )?;
            }
        }
        Ok(())
    }

    fn resolution(
        &mut self,
        discussion: &WorkflowDiscussion,
        root: &str,
        prior: Option<(bool, i64)>,
    ) -> Result<(), StoreError> {
        let Some(resolved) = discussion.resolved else {
            return Ok(());
        };
        let changed = prior.is_none_or(|(previous, _)| previous != resolved);
        let version = next_version(prior.map_or(0, |(_, version)| version), changed)?;
        if changed {
            self.db.prepare_cached(
                "INSERT INTO notification_gitlab_threads VALUES(?1,?2,?3,?4)
                 ON CONFLICT(review_id,root_note_id) DO UPDATE SET resolved=excluded.resolved,version=excluded.version",
            )?.execute(params![self.review.as_str(),root,resolved,version])?;
        }
        if self.last.is_some()
            && prior.is_some()
            && changed
            && !discussion
                .resolved_by
                .is_some_and(|actor| self.reviewers.contains(&actor))
        {
            let kind = if resolved {
                Kind::Resolved
            } else {
                Kind::Reopened
            };
            self.event(
                discussion,
                kind,
                &format!("thread-{root}"),
                None,
                None,
                version,
            )?;
        }
        Ok(())
    }

    fn event(
        &mut self,
        discussion: &WorkflowDiscussion,
        kind: Kind,
        identity: &str,
        note_id: Option<&str>,
        hash: Option<&str>,
        version: i64,
    ) -> Result<(), StoreError> {
        if let Some(id) = observe(
            self.db,
            self.review,
            kind,
            identity,
            Some(note_id.unwrap_or(&discussion.id).to_owned()),
            version,
            self.observed,
        )? {
            self.db
                .prepare_cached("INSERT INTO notification_gitlab_evidence VALUES(?1,?2,?3,?4,?5)")?
                .execute(params![
                    id.as_str(),
                    self.review.as_str(),
                    discussion.id,
                    note_id,
                    hash
                ])?;
            self.events.push(id);
        }
        Ok(())
    }
}

pub(crate) fn occurrence_for(
    db: &Connection,
    review: &ReviewId,
    discussion: &str,
    kind: Kind,
    note: Option<u64>,
    hash: Option<&str>,
    observed: i64,
) -> Result<Option<NotificationId>, StoreError> {
    let found: Option<String> = db.prepare_cached(
        "SELECT n.notification_id FROM notification_gitlab_evidence g JOIN notifications n USING(notification_id)
         WHERE g.review_id=?1 AND g.discussion_id=?2 AND g.note_id IS ?3 AND g.sha256 IS ?4 AND n.kind=?5
           AND n.occurred_at_ms=?6 ORDER BY n.ordinal DESC LIMIT 1",
    )?.query_row(params![review.as_str(),discussion,note.map(|id|id.to_string()),hash,kind.to_string(),observed], |row| row.get(0)).optional()?;
    found.as_deref().map(parse).transpose()
}

fn observe(
    db: &Connection,
    review: &ReviewId,
    kind: Kind,
    identity: &str,
    event_ref: Option<String>,
    version: i64,
    at: i64,
) -> Result<Option<NotificationId>, StoreError> {
    let label = match kind {
        Kind::NewHead => "New code after review",
        Kind::ApprovalWithdrawn => "Your GitLab approval was withdrawn",
        Kind::Reply => "New reply on a review discussion",
        Kind::EditedReply => "A reply on a review discussion changed",
        Kind::Resolved => "A review discussion was resolved",
        Kind::Reopened => "A review discussion was reopened",
        _ => return Err(AppError::Internal.into()),
    };
    insert_notification(
        db,
        Observation {
            source: Source::GitLab,
            occurrence_id: format!("{review}:{identity}:{version}"),
            kind,
            review: review.clone(),
            run: None,
            session: None,
            event_ref,
            label: label.to_owned(),
            occurred_at_ms: at,
            desktop_episode: None,
        },
    )
}

fn next_version(version: i64, changed: bool) -> Result<i64, StoreError> {
    version
        .checked_add(i64::from(changed))
        .ok_or_else(|| AppError::Internal.into())
}
