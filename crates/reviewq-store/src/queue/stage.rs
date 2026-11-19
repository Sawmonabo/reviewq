use std::collections::BTreeSet;

use reviewq_app::events::ResourceKey;
use reviewq_app::sources::WorkflowSnapshot;
use reviewq_domain::identity::{ReviewId, Revision};
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
    let comments = source
        .discussions
        .iter()
        .flat_map(|thread| &thread.notes)
        .filter(|note| reviewers.contains(&note.author))
        .count();
    let changed = tx.sql.prepare_cached(
        "INSERT INTO review_source_signals VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(review_id) DO UPDATE SET settings_revision=excluded.settings_revision,
           source_sha=excluded.source_sha,personal_approval=coalesce(excluded.personal_approval,
             CASE WHEN source_sha=excluded.source_sha AND settings_revision=excluded.settings_revision THEN personal_approval END),
           our_comments=excluded.our_comments,observed_at_ms=excluded.observed_at_ms
         WHERE observed_at_ms<excluded.observed_at_ms
           AND (settings_revision,source_sha,our_comments,
                CASE WHEN excluded.personal_approval IS NOT NULL THEN personal_approval ELSE NULL END)
               IS DISTINCT FROM (excluded.settings_revision,excluded.source_sha,excluded.our_comments,excluded.personal_approval)"
    )?.execute(params![review.as_str(),settings.as_sqlite_integer(),source.review.head_sha.as_str(),
        approval,comments as i64,source.observed_at_ms])?;
    if changed != 0 {
        advance_queue(&tx.sql)?;
        tx.emit(&[
            ResourceKey::Queue,
            ResourceKey::Review(Some(review.clone())),
        ])?;
    }
    Ok(())
}
