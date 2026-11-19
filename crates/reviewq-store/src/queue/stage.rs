use std::collections::BTreeSet;

use reviewq_app::events::ResourceKey;
use reviewq_app::sources::{DiscussionNoteTime, WorkflowSnapshot};
use reviewq_domain::identity::{GitSha, ReviewId, Revision};
use reviewq_domain::review_stage::{WorkspaceEvidence, classify_review, queue_state};
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, params};

use crate::StoreError;
use crate::connection::WriteTx;
use crate::workspace::advance_queue;

pub(crate) fn register(db: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS;
    db.create_scalar_function("reviewq_stage", 19, flags, |args| {
        let newest: Option<String> = args.get(2)?;
        let action: Option<String> = args.get(7)?;
        let intent: Option<String> = args.get(8)?;
        let report: Option<String> = args.get(9)?;
        let followup: Option<String> = args.get(10)?;
        let decision: Option<String> = args.get(16)?;
        let recovery: Option<String> = args.get(18)?;
        let stage = classify_review(&WorkspaceEvidence {
            open: args.get(0)?,
            registered: args.get(1)?,
            newest_activity: newest.as_deref(),
            blocking_question: args.get(3)?,
            any_question: args.get(4)?,
            family_working: args.get(5)?,
            family_failed: args.get(6)?,
            action: action.as_deref(),
            action_intent: intent.as_deref(),
            report_state: report.as_deref(),
            followup_state: followup.as_deref(),
            uncertain_delivery: args.get(11)?,
            personal_approval: args.get(12)?,
            approval_withdrawn: args.get(13)?,
            pending_source: args.get(14)?,
            pending_report: args.get(15)?,
            decision: decision.as_deref(),
            reviewer_comments: args.get(17)?,
            recovery: recovery.as_deref(),
        });
        Ok(stage.map(|stage| format!("{}/{}", stage.stage.as_str(), stage.reason)))
    })?;
    db.create_scalar_function("reviewq_fold", 1, flags, |args| {
        Ok(args.get::<String>(0)?.to_lowercase())
    })?;
    db.create_scalar_function("reviewq_queue_state", 6, flags, |args| {
        let decision: Option<String> = args.get(4)?;
        Ok(queue_state(
            args.get(0)?,
            args.get(1)?,
            args.get(2)?,
            args.get(3)?,
            decision.as_deref(),
            args.get(5)?,
        ))
    })
}

pub(crate) fn observe_source(
    tx: &mut WriteTx<'_>,
    review: &ReviewId,
    settings: Revision,
    source: &WorkflowSnapshot,
    reviewers: &BTreeSet<u64>,
) -> Result<(), StoreError> {
    if tx.sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM notification_gitlab_observations n WHERE n.review_id=?1 AND n.observed_at_ms>?2)",
        params![review.as_str(), source.observed_at_ms],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    let approval = source.reviewer_approval(reviewers);
    let comments = count_discussions(
        source.discussions.iter().map(|thread| {
            (
                thread.resolution,
                thread.notes.iter().map(DiscussionNoteTime::from),
            )
        }),
        reviewers,
    );
    observe_signals(
        tx,
        review,
        settings,
        &source.review.head_sha,
        source.observed_at_ms,
        approval,
        Some(comments),
    )
}

pub(crate) struct ObservedComments {
    count: i64,
    total: i64,
    open: Option<i64>,
    latest_at: Option<i64>,
}

pub(crate) fn count_discussions<A: IntoIterator<Item = DiscussionNoteTime>>(
    threads: impl Iterator<Item = (reviewq_app::changes::DiscussionResolution, A)>,
    reviewers: &BTreeSet<u64>,
) -> ObservedComments {
    use reviewq_app::changes::DiscussionResolution;
    let mut result = ObservedComments {
        count: 0,
        total: 0,
        open: Some(0),
        latest_at: None,
    };
    for (resolution, notes) in threads {
        let mut ours = 0;
        for note in notes
            .into_iter()
            .filter(|note| reviewers.contains(&note.author))
        {
            ours += 1;
            result.latest_at = result.latest_at.max(Some(note.created_at_ms));
        }
        if ours == 0 {
            continue;
        }
        result.count += ours;
        result.total += 1;
        match resolution {
            DiscussionResolution::Unknown => result.open = None,
            DiscussionResolution::Open => result.open = result.open.map(|open| open + 1),
            _ => {}
        }
    }
    result
}

pub(crate) fn observe_signals(
    tx: &mut WriteTx<'_>,
    review: &ReviewId,
    settings: Revision,
    head: &GitSha,
    observed_at: i64,
    approval: Option<bool>,
    comments: Option<ObservedComments>,
) -> Result<(), StoreError> {
    let comments = comments.as_ref();
    let changed = tx.sql.prepare_cached(
        "INSERT INTO review_source_signals
            (review_id, settings_revision, source_sha, personal_approval, our_comments,
             observed_at_ms, our_threads, open_threads)
         VALUES (?1, ?2, ?3, ?4, coalesce(?5,0), ?6, ?7, ?8)
         ON CONFLICT(review_id) DO UPDATE SET
           settings_revision = ?2, source_sha = ?3, observed_at_ms = ?6,
           personal_approval = coalesce(?4,
             CASE WHEN (settings_revision,source_sha)=(?2,?3) THEN personal_approval END),
           our_comments = coalesce(?5,
             CASE WHEN (settings_revision,source_sha)=(?2,?3) THEN our_comments END, 0),
           our_threads = coalesce(?7,
             CASE WHEN (settings_revision,source_sha)=(?2,?3) THEN our_threads END),
           open_threads = CASE WHEN ?7 IS NOT NULL THEN ?8
             WHEN (settings_revision,source_sha)=(?2,?3) THEN open_threads END
         WHERE (observed_at_ms < ?6 OR (observed_at_ms = ?6 AND our_threads IS NULL AND ?7 IS NOT NULL))
           AND ((settings_revision,source_sha) <> (?2,?3)
             OR (?4 IS NOT NULL AND personal_approval IS NOT ?4)
             OR (?7 IS NOT NULL AND (our_comments,our_threads,open_threads) IS NOT (?5,?7,?8)))"
    )?.execute(params![
        review.as_str(), settings.as_sqlite_integer(), head.as_str(), approval,
        comments.map(|value| value.count), observed_at,
        comments.map(|value| value.total), comments.and_then(|value| value.open),
    ])?;
    let handoff_changed = comments
        .map(|comments| {
            observe_reviewer_activity(
                &tx.sql,
                review,
                settings,
                head,
                observed_at,
                comments.latest_at,
            )
        })
        .transpose()?
        .unwrap_or(false);
    if changed == 0 {
        // A later, identical snapshot must still fence out older observations.
        tx.sql.execute(
            "UPDATE review_source_signals SET observed_at_ms=?2
                        WHERE review_id=?1 AND observed_at_ms<?2",
            params![review.as_str(), observed_at],
        )?;
    }
    if changed != 0 || handoff_changed {
        advance_queue(&tx.sql)?;
        tx.emit(&[
            ResourceKey::Queue,
            ResourceKey::Review(Some(review.clone())),
        ])?;
    }
    Ok(())
}

fn observe_reviewer_activity(
    db: &Connection,
    review: &ReviewId,
    settings: Revision,
    head: &GitSha,
    observed_at: i64,
    latest_comment: Option<i64>,
) -> Result<bool, StoreError> {
    let changed = db.prepare_cached(
        "INSERT INTO review_reviewer_activity VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(review_id) DO UPDATE SET settings_revision=?2,source_sha=?3,
            latest_comment_at_ms=?4,observed_at_ms=?5
         WHERE observed_at_ms<=?5 AND (settings_revision,source_sha,latest_comment_at_ms) IS NOT (?2,?3,?4)",
    )?.execute(params![review.as_str(), settings.as_sqlite_integer(), head.as_str(), latest_comment, observed_at])? != 0;
    if !changed {
        db.prepare_cached(
            "UPDATE review_reviewer_activity SET observed_at_ms=?2 WHERE review_id=?1 AND observed_at_ms<?2",
        )?.execute(params![review.as_str(), observed_at])?;
    }
    Ok(changed)
}
