use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::followups::{
    FollowupCommands, MonitorCommand, MonitorReceipt, MonitorReplay, MonitorRequest, MonitorState,
    MonitoredReview, Preferences, UpdatePreferences,
};
use reviewq_app::queue::MergeRequestState;
use reviewq_app::sources::WorkflowSnapshot;
use reviewq_domain::identity::{MrIdentity, ReviewId, Revision, SubmissionId};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use super::{authorize, observations, preferences, revision};
use crate::connection::WriteTx;
use crate::decisions::{record_receipt, verify_replay};
use crate::events::{now_ms, snapshot};
use crate::queue::{identity_for_review, lead_for_run, parse, text};
use crate::workspace::{
    actor_key, advance_queue, configured, hash_field, operator, settings_revision,
};
use crate::{SqliteStore, StoreError};

const SET_PREFERENCES: &str = "followup_preferences";
const MONITOR: &str = "followup_monitor";

impl FollowupCommands for SqliteStore {
    fn save_preferences(
        &self,
        actor: Actor,
        input: UpdatePreferences,
    ) -> AppFuture<'_, Snapshot<Preferences>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            if input.concurrency.is_none() && input.automatic.is_none()
                || input
                    .concurrency
                    .is_some_and(|value| !(1..=8).contains(&value))
            {
                return Err(AppError::InvalidRequest);
            }
            self.write(move |tx| save_preferences(tx, actor, input))
                .await
                .map_err(StoreError::app)
        })
    }

    fn read_monitor_replay(
        &self,
        actor: Actor,
        input: MonitorReplay,
    ) -> AppFuture<'_, Option<Snapshot<MonitorReceipt>>> {
        Box::pin(async move {
            self.read(move |db| {
                let (review, submission, digest) = match &input {
                    MonitorReplay::Resolved(input) => (
                        Some(&input.review),
                        &input.submission,
                        monitor_digest(input),
                    ),
                    MonitorReplay::Original { submission, digest } => (None, submission, *digest),
                };
                replay(db, &actor, review, submission, &digest)?
                    .map(|receipt| snapshot(db, receipt))
                    .transpose()
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn monitor_source(
        &self,
        actor: Actor,
        review: ReviewId,
    ) -> AppFuture<'_, (MrIdentity, Revision)> {
        Box::pin(async move {
            self.read(move |db| {
                authorize_command(db, &actor, &review)?;
                let identity = identity_for_review(db, &review)?;
                Ok((identity, settings_revision(db)?))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn commit_monitor(
        &self,
        actor: Actor,
        input: MonitorRequest,
        settings: Revision,
        observation: Option<WorkflowSnapshot>,
    ) -> AppFuture<'_, Snapshot<MonitorReceipt>> {
        Box::pin(async move {
            self.write(move |tx| monitor(tx, actor, input, settings, observation))
                .await
                .map_err(StoreError::app)
        })
    }
}

fn save_preferences(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    input: UpdatePreferences,
) -> Result<Snapshot<Preferences>, StoreError> {
    let db = &tx.sql;
    let key = actor_key(&actor);
    let mut digest = Sha256::new();
    digest.update(input.expected_revision.get().to_be_bytes());
    digest.update([
        input.automatic.map_or(0, |value| if value { 2 } else { 1 }),
        input.concurrency.unwrap_or(0),
    ]);
    let digest = digest.finalize().into();
    if verify_replay(db, &key, SET_PREFERENCES, &input.submission, &digest)? {
        let result = db.query_row(
            "SELECT automatic, active_limit, generation FROM followup_command_receipts
             WHERE actor_key=?1 AND operation=?2 AND submission_id=?3",
            params![key.as_slice(), SET_PREFERENCES, input.submission.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        return snapshot(
            db,
            Preferences {
                automatic: result.0,
                concurrency: result.1,
                revision: revision(result.2)?,
            },
        );
    }
    let mut current = preferences(db)?;
    if current.revision != input.expected_revision {
        return Err(AppError::StaleRevision.into());
    }
    current.revision = current.revision.checked_add(1).ok_or(AppError::Internal)?;
    current.automatic = input.automatic.unwrap_or(current.automatic);
    current.concurrency = input.concurrency.unwrap_or(current.concurrency);
    db.execute(
        "INSERT INTO followup_preferences(singleton, revision, automatic, active_limit) VALUES(1,?1,?2,?3)
         ON CONFLICT(singleton) DO UPDATE SET revision=excluded.revision,
             automatic=excluded.automatic, active_limit=excluded.active_limit",
        params![current.revision.as_sqlite_integer(), current.automatic, current.concurrency],
    )?;
    record_receipt(db, &actor, SET_PREFERENCES, &input.submission, &digest)?;
    db.execute(
        "INSERT INTO followup_command_receipts(actor_key, operation, submission_id, generation, automatic, active_limit)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![key.as_slice(), SET_PREFERENCES, input.submission.as_str(),
            current.revision.as_sqlite_integer(), current.automatic, current.concurrency],
    )?;
    tx.emit(&[ResourceKey::Settings, ResourceKey::Followup(None)])?;
    snapshot(&tx.sql, current)
}

fn authorize_command(db: &Connection, actor: &Actor, review: &ReviewId) -> Result<(), StoreError> {
    if matches!(actor.scope(), ActorScope::NativeSession(_)) {
        return Err(AppError::Forbidden.into());
    }
    authorize(db, actor, review)
}

fn validated_observation(
    db: &Connection,
    review: &ReviewId,
    observed: Option<WorkflowSnapshot>,
) -> Result<WorkflowSnapshot, StoreError> {
    let value = observed.ok_or(AppError::Unavailable)?;
    let identity = identity_for_review(db, review)?;
    let (head, state): (String, String) = db.query_row(
        "SELECT head_sha, state FROM reviews WHERE review_id=?1",
        [review.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if value.review.identity != identity
        || !configured(db, &identity)?
        || head != value.review.head_sha.as_str()
        || state != "open"
        || value.review.state != MergeRequestState::Open
    {
        return Err(AppError::StaleRevision.into());
    }
    Ok(value)
}

fn replay(
    db: &Connection,
    actor: &Actor,
    review: Option<&ReviewId>,
    submission: &SubmissionId,
    digest: &[u8; 32],
) -> Result<Option<MonitorReceipt>, StoreError> {
    if let Some(review) = review {
        authorize_command(db, actor, review)?;
    }
    let key = actor_key(actor);
    let mut statement = db.prepare_cached(
        "SELECT review_id, generation, state, batch_id FROM followup_command_receipts
         WHERE actor_key=?1 AND operation=?2 AND submission_id=?3",
    )?;
    let mut rows = statement.query(params![key.as_slice(), MONITOR, submission.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let saved_review = parse(text(row, 0)?)?;
    if review != Some(&saved_review) {
        authorize_command(db, actor, &saved_review)?;
    }
    if !verify_replay(db, &key, MONITOR, submission, digest)? {
        return Err(AppError::Internal.into());
    }
    Ok(Some(MonitorReceipt {
        review: saved_review,
        submission: submission.clone(),
        generation: revision(row.get(1)?)?,
        state: parse(text(row, 2)?)?,
        batch_id: row.get(3)?,
    }))
}

fn monitor_digest(input: &MonitorRequest) -> [u8; 32] {
    if let Some(original) = &input.original {
        return original.digest;
    }
    let mut digest = Sha256::new();
    hash_field(&mut digest, input.review.as_str().as_bytes());
    digest.update(
        input
            .expected_generation
            .map_or(0, Revision::get)
            .to_be_bytes(),
    );
    match &input.command {
        MonitorCommand::Configure {
            run,
            state,
            selections,
        } => {
            hash_field(&mut digest, b"configure");
            hash_field(&mut digest, run.as_str().as_bytes());
            hash_field(&mut digest, state.to_string().as_bytes());
            digest.update((selections.len() as u64).to_be_bytes());
            for selection in selections {
                hash_field(&mut digest, selection.discussion_id.as_bytes());
                digest.update((selection.original_note_ids.len() as u64).to_be_bytes());
                for id in &selection.original_note_ids {
                    digest.update(id.to_be_bytes());
                }
                digest.update((selection.finding_ids.len() as u64).to_be_bytes());
                for finding in &selection.finding_ids {
                    hash_field(&mut digest, finding.as_bytes());
                }
            }
        }
        MonitorCommand::Pause => hash_field(&mut digest, b"pause"),
        MonitorCommand::Resume => hash_field(&mut digest, b"resume"),
        MonitorCommand::ReviewNow => hash_field(&mut digest, b"review_now"),
    }
    digest.finalize().into()
}

struct CurrentMonitor {
    run: Option<String>,
    generation: Revision,
    state: MonitorState,
    verified: bool,
    reviewer_id: Option<String>,
    policy_override: Option<bool>,
}

fn current(db: &Connection, review: &ReviewId) -> Result<Option<CurrentMonitor>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT run_id, generation, state, binding_verified, reviewer_id, policy_override FROM followup_monitors WHERE review_id=?1",
    )?;
    let mut rows = statement.query([review.as_str()])?;
    rows.next()?
        .map(|row| {
            Ok(CurrentMonitor {
                run: row.get(0)?,
                generation: revision(row.get(1)?)?,
                state: parse(text(row, 2)?)?,
                verified: row.get(3)?,
                reviewer_id: row.get(4)?,
                policy_override: row.get(5)?,
            })
        })
        .transpose()
}

fn monitor(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    input: MonitorRequest,
    settings: Revision,
    observation: Option<WorkflowSnapshot>,
) -> Result<Snapshot<MonitorReceipt>, StoreError> {
    let db = &tx.sql;
    let digest = monitor_digest(&input);
    if let Some(previous) = replay(db, &actor, Some(&input.review), &input.submission, &digest)? {
        return snapshot(db, previous);
    }
    let previous = current(db, &input.review)?;
    if let Some(run) = input
        .original
        .as_ref()
        .and_then(|value| value.transition_run.as_ref())
        && previous.as_ref().and_then(|value| value.run.as_deref()) != Some(run.as_str())
    {
        return Err(AppError::Conflict.into());
    }
    if settings != settings_revision(db)?
        || input.expected_generation != previous.as_ref().map(|value| value.generation)
    {
        return Err(AppError::StaleRevision.into());
    }
    let time = now_ms()?;
    let mut new_notifications = Vec::new();
    let mut changed = true;
    let (generation, state, batch_id) = match &input.command {
        MonitorCommand::Configure {
            run,
            state,
            selections,
        } => {
            let lead = lead_for_run(db, run)?.ok_or(AppError::NotFound)?;
            if lead.review_id() != &input.review {
                return Err(AppError::NotFound.into());
            }
            let observation = validated_observation(db, &input.review, observation)?;
            let reviewers = observations::monitored_reviewers(db, observation.authenticated_actor)?;
            if observation.reviewer_approval(&reviewers) != Some(false)
                && *state == MonitorState::Enabled
            {
                return Err(AppError::Conflict.into());
            }
            let selected = observations::verified_threads(&observation, selections, &reviewers)?;
            new_notifications =
                crate::notifications::gitlab::collect(db, &input.review, &observation, &reviewers)?;
            let mut observed_followups = false;
            let owner = observation.authenticated_actor.to_string();
            if let Some(old) = &previous
                && old.verified
                && old.reviewer_id.as_deref() == Some(owner.as_str())
                && let Some(run_id) = old.run.as_deref()
            {
                let target = MonitoredReview {
                    review: input.review.clone(),
                    run: parse(run_id)?,
                    generation: old.generation,
                    identity: observation.review.identity.clone(),
                    settings,
                };
                observed_followups =
                    observations::adopt_remote(db, &target, &observation, &reviewers, time)?;
            }
            let same = if let Some(old) = &previous {
                old.verified
                    && old.reviewer_id.as_deref() == Some(owner.as_str())
                    && old.run.as_deref() == Some(run.as_str())
                    && old.state == *state
                    && old.policy_override == Some(*state == MonitorState::Enabled)
                    && observations::same_enrollment(db, &input.review, old.generation, &selected)?
            } else {
                false
            };
            if same {
                changed = observed_followups;
                (
                    previous.as_ref().ok_or(AppError::Internal)?.generation,
                    *state,
                    None,
                )
            } else {
                let generation = previous
                    .as_ref()
                    .map_or(revision(0)?, |value| value.generation)
                    .checked_add(1)
                    .ok_or(AppError::Internal)?;
                db.execute(
                "INSERT INTO followup_monitors(review_id, run_id, generation, state, policy_override,
                     binding_verified, updated_at_ms, reviewer_id, last_complete_observation_ms) VALUES(?1,?2,?3,?4,?5,1,?6,?7,?8)
                 ON CONFLICT(review_id) DO UPDATE SET run_id=excluded.run_id, generation=excluded.generation,
                     state=excluded.state, policy_override=excluded.policy_override,
                     binding_verified=1, reason=NULL, updated_at_ms=excluded.updated_at_ms,
                     reviewer_id=excluded.reviewer_id,last_complete_observation_ms=excluded.last_complete_observation_ms",
                params![input.review.as_str(), run.as_str(), generation.as_sqlite_integer(),
                    state.to_string(), *state == MonitorState::Enabled, time, observation.authenticated_actor.to_string(), observation.observed_at_ms],
            )?;
                observations::save_selection(db, &input.review, generation, selected)?;
                if let Some(old) = previous {
                    db.execute(
                        "INSERT INTO followup_threads(review_id,generation,original_note_id,discussion_id,
                            reviewer_id,selected,discussion_version,baseline_sha256,resolved)
                         SELECT review_id,?3,original_note_id,discussion_id,reviewer_id,0,discussion_version,
                            baseline_sha256,resolved FROM followup_threads
                         WHERE review_id=?1 AND generation=?2 ON CONFLICT DO NOTHING",
                        params![input.review.as_str(),old.generation.as_sqlite_integer(),generation.as_sqlite_integer()],
                    )?;
                    observations::carry_pending(
                        db,
                        &input.review,
                        old.generation,
                        generation,
                        run,
                        time,
                    )?;
                }
                observations::add_reviewer_threads(
                    db,
                    &input.review,
                    generation,
                    &observation,
                    &reviewers,
                    None,
                    false,
                )?;
                (generation, *state, None)
            }
        }
        MonitorCommand::Pause | MonitorCommand::Resume => {
            let previous = previous.ok_or(AppError::NotFound)?;
            let pause = matches!(input.command, MonitorCommand::Pause);
            if !pause {
                let observation = validated_observation(db, &input.review, observation)?;
                let reviewers =
                    observations::monitored_reviewers(db, observation.authenticated_actor)?;
                if observation.reviewer_approval(&reviewers) != Some(false)
                    || !previous.verified
                    || previous.reviewer_id.as_deref()
                        != Some(observation.authenticated_actor.to_string().as_str())
                {
                    return Err(AppError::Conflict.into());
                }
                observations::require_known_threads(
                    db,
                    &input.review,
                    previous.generation,
                    &observation,
                    &reviewers,
                )?;
            }
            if pause {
                db.execute(
                    "UPDATE followup_batches SET manual_requested=0 WHERE review_id=?1 AND authority_verified=1
                       AND state IN ('pending','reserved') AND manual_requested=1",
                    [input.review.as_str()],
                )?;
            }
            if pause && previous.state == MonitorState::Archived {
                return Err(AppError::Conflict.into());
            }
            let next = if pause {
                MonitorState::Paused
            } else {
                MonitorState::Enabled
            };
            db.execute(
                "UPDATE followup_monitors SET state=?2, policy_override=?3, reason=NULL,
                     updated_at_ms=max(updated_at_ms,?4) WHERE review_id=?1",
                params![input.review.as_str(), next.to_string(), !pause, time],
            )?;
            (previous.generation, next, None)
        }
        MonitorCommand::ReviewNow => {
            let previous = previous.ok_or(AppError::NotFound)?;
            let live = validated_observation(db, &input.review, observation)?;
            let reviewers = observations::monitored_reviewers(db, live.authenticated_actor)?;
            if previous.state == MonitorState::Archived
                || !previous.verified
                || live.reviewer_approval(&reviewers) != Some(false)
                || previous.reviewer_id.as_deref()
                    != Some(live.authenticated_actor.to_string().as_str())
            {
                return Err(AppError::Conflict.into());
            }
            let run = previous
                .run
                .as_deref()
                .map(parse)
                .transpose()?
                .ok_or(AppError::NotFound)?;
            observations::require_known_threads(
                db,
                &input.review,
                previous.generation,
                &live,
                &reviewers,
            )?;
            let batch = observations::review_now(
                db,
                &input.review,
                &run,
                previous.generation,
                input.submission.as_str(),
                &live,
                time,
            )?;
            (previous.generation, previous.state, Some(batch))
        }
    };
    let key = record_receipt(db, &actor, MONITOR, &input.submission, &digest)?;
    db.execute(
        "INSERT INTO followup_command_receipts(actor_key, operation, submission_id, review_id, generation, state, batch_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![key.as_slice(), MONITOR, input.submission.as_str(), input.review.as_str(),
            generation.as_sqlite_integer(), state.to_string(), batch_id],
    )?;
    crate::notifications::publish(tx, &new_notifications)?;
    if changed {
        advance_queue(&tx.sql)?;
        let keys = [
            ResourceKey::Followup(Some(input.review.clone())),
            ResourceKey::Review(Some(input.review.clone())),
            ResourceKey::Queue,
        ];
        tx.emit(&keys)?;
    }
    snapshot(
        &tx.sql,
        MonitorReceipt {
            review: input.review,
            submission: input.submission,
            generation,
            state,
            batch_id,
        },
    )
}
