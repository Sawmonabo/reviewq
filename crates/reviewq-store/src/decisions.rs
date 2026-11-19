//! Exact source and revision checks for local decisions.

use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::decisions::{
    Decision, DecisionRequest, DecisionResult, DecisionStore, Disposition, Frontier,
    ObservedAttention, ReportFrontier, SessionFrontier, TerminalDecision,
};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::queue::{MergeRequestState, ReviewSource, RunSummary};
use reviewq_domain::identity::{MrIdentity, ReviewId, Revision, SubmissionId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use crate::events::{now_ms, snapshot};
use crate::queue::{identity_for_review, parse, save_observations, source_for_review, text};
use crate::workspace::{
    actor_key, advance_queue, allow_review, configured, hash_field, scope, settings_revision,
};
use crate::{SqliteStore, StoreError};

const DECISION: &str = "review_decision";
const CHECKPOINT: &str = "review_checkpoint";
const TERMINAL: &str = "terminal_review_decision";

impl DecisionStore for SqliteStore {
    fn terminal_decision_replay(
        &self,
        actor: Actor,
        input: TerminalDecision,
    ) -> AppFuture<'_, Option<Snapshot<DecisionResult>>> {
        Box::pin(async move {
            crate::workspace::operator(&actor).map_err(StoreError::app)?;
            self.read(move |db| {
                let key = actor_key(&actor);
                let digest = terminal_digest(&input.target, &input.source_sha, input.disposition);
                if !verify_replay(db, &key, TERMINAL, &input.submission, &digest)? {
                    return Ok(None);
                }
                let result = read_receipt(db, &key, TERMINAL, &input.submission)?;
                authorized(db, &actor, &result.review)?;
                Ok(Some(snapshot(db, result)?))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_decision_replay(
        &self,
        actor: Actor,
        input: DecisionRequest,
    ) -> AppFuture<'_, Option<Snapshot<DecisionResult>>> {
        Box::pin(async move {
            self.read(move |db| {
                authorized(db, &actor, &input.review)?;
                replay(db, &actor, &input)?
                    .map(|value| snapshot(db, value))
                    .transpose()
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn decision_source(&self, actor: Actor, review: ReviewId) -> AppFuture<'_, ReviewSource> {
        Box::pin(async move {
            self.read(move |db| {
                let identity = authorized(db, &actor, &review)?;
                if !configured(db, &identity)? {
                    return Err(AppError::Unavailable.into());
                }
                source_for_review(db, &review)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn commit_decision(
        &self,
        actor: Actor,
        input: DecisionRequest,
        prior: ReviewSource,
        observed: ReviewSource,
        checkpoint: Option<Frontier>,
    ) -> AppFuture<'_, Snapshot<DecisionResult>> {
        Box::pin(async move {
            self.write(move |tx| {
                let db = &tx.sql;
                let current_identity = authorized(db, &actor, &input.review)?;
                if let Some(previous) = replay(db, &actor, &input)? {
                    return snapshot(db, previous);
                }
                if current_identity != prior.identity
                    || current_identity != observed.identity
                    || !configured(db, &current_identity)?
                    || settings_revision(db)? != input.expected_settings
                {
                    return Err(AppError::StaleRevision.into());
                }
                if input.checkpoint_id.is_some() != checkpoint.is_some()
                    || (checkpoint.is_none() && input.disposition.is_none())
                    || (input.terminal_target.is_some()
                        && (checkpoint.is_some()
                            || input.disposition.is_none()
                            || !matches!(actor.scope(), ActorScope::LocalOperator)))
                {
                    return Err(AppError::InvalidRequest.into());
                }
                let source = source_for_review(db, &input.review)?;
                if observed.head_sha != input.source_sha
                    || observed.state != MergeRequestState::Open
                    || (source != prior && source != observed)
                    || (observed.updated_at_ms < source.updated_at_ms
                        && observed.head_sha != source.head_sha)
                {
                    return Err(AppError::StaleRevision.into());
                }
                let mut outcome = current(db, &input.review)?;
                if outcome.revision != input.expected_revision {
                    return Err(AppError::StaleRevision.into());
                }
                save_observations(tx, std::slice::from_ref(&observed))?;
                let db = &tx.sql;
                outcome.revision = outcome.revision.checked_add(1).ok_or(AppError::Internal)?;
                if let Some(disposition) = input.disposition {
                    outcome.disposition = Some(disposition);
                    let covered = disposition != Disposition::NeedsReview;
                    outcome.reviewed_sha = covered.then(|| input.source_sha.clone());
                    outcome.reviewed_at_ms = covered.then(now_ms).transpose()?;
                }
                if let Some(frontier) = checkpoint {
                    mark_handled(db, &input.review, frontier)?;
                }
                record(db, &actor, &input, &outcome)?;
                advance_queue(db)?;
                tx.emit(&[
                    ResourceKey::Review(Some(input.review.clone())),
                    ResourceKey::Queue,
                ])?;
                snapshot(
                    &tx.sql,
                    DecisionResult {
                        review: input.review,
                        decision: outcome,
                        checkpoint_id: input.checkpoint_id,
                    },
                )
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn authorized(db: &Connection, actor: &Actor, review: &ReviewId) -> Result<MrIdentity, StoreError> {
    if matches!(actor.scope(), ActorScope::NativeSession(_)) {
        return Err(AppError::Forbidden.into());
    }
    let access = scope(db, actor)?;
    let identity = identity_for_review(db, review)?;
    allow_review(db, access, review, &identity)?;
    Ok(identity)
}

fn operation(input: &DecisionRequest) -> &'static str {
    if input.terminal_target.is_some() {
        TERMINAL
    } else if input.disposition.is_some() {
        DECISION
    } else {
        CHECKPOINT
    }
}

fn digest(input: &DecisionRequest) -> [u8; 32] {
    if let (Some(target), Some(disposition)) = (&input.terminal_target, input.disposition) {
        return terminal_digest(target, &input.source_sha, disposition);
    }
    let mut digest = Sha256::new();
    hash_field(&mut digest, input.review.as_str().as_bytes());
    hash_field(&mut digest, input.source_sha.as_str().as_bytes());
    hash_field(&mut digest, operation(input).as_bytes());
    hash_field(
        &mut digest,
        input.checkpoint_id.as_deref().unwrap_or("").as_bytes(),
    );
    if let Some(choice) = input.disposition {
        hash_field(&mut digest, choice.to_string().as_bytes());
    }
    digest.update(input.expected_revision.get().to_be_bytes());
    digest.update(input.expected_settings.get().to_be_bytes());
    digest.finalize().into()
}

fn terminal_digest(
    target: &str,
    sha: &reviewq_domain::identity::GitSha,
    disposition: Disposition,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    hash_field(&mut digest, target.as_bytes());
    hash_field(&mut digest, sha.as_str().as_bytes());
    hash_field(&mut digest, disposition.to_string().as_bytes());
    digest.finalize().into()
}

fn replay(
    db: &Connection,
    actor: &Actor,
    input: &DecisionRequest,
) -> Result<Option<DecisionResult>, StoreError> {
    let key = actor_key(actor);
    if !verify_replay(
        db,
        &key,
        operation(input),
        &input.submission,
        &digest(input),
    )? {
        return Ok(None);
    }
    Ok(Some(read_receipt(
        db,
        &key,
        operation(input),
        &input.submission,
    )?))
}

fn read_receipt(
    db: &Connection,
    key: &[u8; 32],
    operation: &str,
    submission: &SubmissionId,
) -> Result<DecisionResult, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT review_id, revision, disposition, reviewed_sha, reviewed_at_ms, checkpoint_id
         FROM review_decision_receipts WHERE actor_key=?1 AND operation=?2 AND submission_id=?3",
    )?;
    let mut rows = statement.query(params![key.as_slice(), operation, submission.as_str()])?;
    let row = rows.next()?.ok_or(AppError::Internal)?;
    Ok(DecisionResult {
        review: parse(text(row, 0)?)?,
        decision: decode(row, 1)?,
        checkpoint_id: row.get(5)?,
    })
}

fn record(
    db: &Connection,
    actor: &Actor,
    input: &DecisionRequest,
    next: &Decision,
) -> Result<(), StoreError> {
    let key = record_receipt(
        db,
        actor,
        operation(input),
        &input.submission,
        &digest(input),
    )?;
    let disposition = next.disposition.map(|value| value.to_string());
    let reviewed_sha = next.reviewed_sha.as_ref().map(|value| value.as_str());
    db.execute(
        "INSERT INTO review_decisions(review_id, revision, disposition, reviewed_sha, reviewed_at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(review_id) DO UPDATE SET revision=excluded.revision,
            disposition=excluded.disposition, reviewed_sha=excluded.reviewed_sha, reviewed_at_ms=excluded.reviewed_at_ms",
        params![input.review.as_str(), next.revision.as_sqlite_integer(), disposition, reviewed_sha, next.reviewed_at_ms],
    )?;
    db.execute(
        "INSERT INTO review_decision_receipts(actor_key, operation, submission_id, review_id,
              revision, disposition, reviewed_sha, reviewed_at_ms, checkpoint_id)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            key.as_slice(),
            operation(input),
            input.submission.as_str(),
            input.review.as_str(),
            next.revision.as_sqlite_integer(),
            disposition,
            reviewed_sha,
            next.reviewed_at_ms,
            input.checkpoint_id
        ],
    )?;
    Ok(())
}

pub(crate) fn verify_replay(
    db: &Connection,
    key: &[u8; 32],
    operation: &str,
    submission: &SubmissionId,
    digest: &[u8; 32],
) -> Result<bool, StoreError> {
    let previous = db.query_row(
        "SELECT request_digest FROM mutation_receipts WHERE actor_key=?1 AND operation=?2 AND submission_id=?3",
        params![key.as_slice(), operation, submission.as_str()], |row| row.get::<_, Vec<u8>>(0),
    ).optional()?;
    if previous
        .as_deref()
        .is_some_and(|previous| previous != digest)
    {
        return Err(AppError::IdempotencyConflict.into());
    }
    Ok(previous.is_some())
}

pub(crate) fn record_receipt(
    db: &Connection,
    actor: &Actor,
    operation: &str,
    submission: &SubmissionId,
    digest: &[u8; 32],
) -> Result<[u8; 32], StoreError> {
    let key = actor_key(actor);
    let grant = match actor.scope() {
        ActorScope::CheckoutGrant(id) => Some(id.as_str()),
        _ => None,
    };
    db.execute(
        "INSERT INTO mutation_receipts(actor_key, operation, submission_id, request_digest, grant_id, grant_epoch)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![key.as_slice(), operation, submission.as_str(), digest.as_slice(), grant,
            actor.grant_epoch().map(Revision::as_sqlite_integer)],
    )?;
    Ok(key)
}

pub(crate) fn current(db: &Connection, review: &ReviewId) -> Result<Decision, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT revision, disposition, reviewed_sha, reviewed_at_ms FROM review_decisions WHERE review_id=?1",
    )?;
    let mut rows = statement.query([review.as_str()])?;
    match rows.next()? {
        Some(row) => decode(row, 0),
        None => Ok(Decision {
            revision: revision(0)?,
            disposition: None,
            reviewed_sha: None,
            reviewed_at_ms: None,
        }),
    }
}

pub(crate) fn decode(row: &Row<'_>, column: usize) -> Result<Decision, StoreError> {
    Ok(Decision {
        revision: revision(row.get(column)?)?,
        disposition: row
            .get::<_, Option<String>>(column + 1)?
            .as_deref()
            .map(parse)
            .transpose()?,
        reviewed_sha: row
            .get::<_, Option<String>>(column + 2)?
            .as_deref()
            .map(parse)
            .transpose()?,
        reviewed_at_ms: row.get(column + 3)?,
    })
}

fn revision(value: i64) -> Result<Revision, StoreError> {
    Revision::try_from(value).map_err(|_| AppError::Internal.into())
}

pub(crate) fn capture_frontier(
    db: &Connection,
    review: &ReviewId,
    visible_runs: &[RunSummary],
) -> Result<Frontier, StoreError> {
    let mut source = db.prepare_cached(
        "SELECT ordinal, notification_id, kind, label FROM notifications n
         WHERE n.review_id=?1 AND n.source='gitlab' AND n.ordinal>coalesce(
            (SELECT source_ordinal FROM review_handled_frontiers WHERE review_id=?1), 0)
         ORDER BY n.ordinal LIMIT 20",
    )?;
    let mut source_rows = source.query([review.as_str()])?;
    let mut frontier = Frontier::default();
    while let Some(row) = source_rows.next()? {
        frontier.source.push(attention(row)?);
    }
    let mut reports = db.prepare_cached(
        "SELECT v.receipt_id, v.attention_receipt_id FROM report_heads h
         JOIN report_versions v USING(run_id, receipt_id)
         WHERE h.run_id=?1 AND NOT EXISTS (SELECT 1 FROM review_handled_reports handled
            WHERE handled.review_id=?2 AND handled.run_id=h.run_id
              AND handled.attention_receipt_id=v.attention_receipt_id)",
    )?;
    let mut sessions = db.prepare_cached(
        "SELECT ordinal, notification_id, kind, label FROM notifications n
         WHERE n.review_id=?1 AND n.run_id=?2 AND n.session_key=?3
            AND n.source IN ('codex', 'claude') AND n.ordinal>coalesce((
                SELECT ordinal FROM review_handled_sessions h
                WHERE h.review_id=?1 AND h.run_id=?2 AND h.session_key=?3
            ), 0)
         ORDER BY n.ordinal LIMIT 3",
    )?;
    for run in visible_runs {
        let mut report = reports.query(params![run.run_id.as_str(), review.as_str()])?;
        if let Some(row) = report.next()? {
            frontier.reports.push(ReportFrontier {
                run: run.run_id.clone(),
                receipt: parse(text(row, 0)?)?,
                attention: parse(text(row, 1)?)?,
            });
        }
        let mut rows = sessions.query(params![
            review.as_str(),
            run.run_id.as_str(),
            run.lead_session_key.as_str()
        ])?;
        while let Some(row) = rows.next()? {
            frontier.sessions.push(SessionFrontier {
                run: run.run_id.clone(),
                session: run.lead_session_key.clone(),
                event: attention(row)?,
            });
        }
    }
    Ok(frontier)
}

fn attention(row: &Row<'_>) -> Result<ObservedAttention, StoreError> {
    Ok(ObservedAttention {
        ordinal: row.get(0)?,
        id: parse(text(row, 1)?)?,
        kind: parse(text(row, 2)?)?,
        label: row.get(3)?,
    })
}

fn mark_handled(db: &Connection, review: &ReviewId, frontier: Frontier) -> Result<(), StoreError> {
    if let Some(ordinal) = frontier.source.last().map(|event| event.ordinal) {
        db.execute(
            "INSERT INTO review_handled_frontiers(review_id, source_ordinal) VALUES(?1, ?2)
             ON CONFLICT(review_id) DO UPDATE SET source_ordinal=max(coalesce(source_ordinal, 0), excluded.source_ordinal)",
            params![review.as_str(), ordinal],
        )?;
    }
    for report in frontier.reports {
        db.prepare_cached(
            "INSERT INTO review_handled_reports(review_id, run_id, attention_receipt_id) VALUES(?1, ?2, ?3)
             ON CONFLICT DO NOTHING",
        )?.execute(params![review.as_str(), report.run.as_str(), report.attention.as_str()])?;
    }
    for session in frontier.sessions {
        db.prepare_cached(
            "INSERT INTO review_handled_sessions(review_id, run_id, session_key, ordinal)
             VALUES(?1, ?2, ?3, ?4) ON CONFLICT(review_id, run_id, session_key) DO UPDATE
             SET ordinal=max(ordinal, excluded.ordinal)",
        )?
        .execute(params![
            review.as_str(),
            session.run.as_str(),
            session.session.as_str(),
            session.event.ordinal
        ])?;
    }
    Ok(())
}
