//! Exact lead registration, scoped admissions, and fenced delivery outcomes.

use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::hosts::{
    DeliveryClaim, DeliveryObservation, DeliveryReceipt, DeliveryRecoveryCursor, HostActivity,
    HostPhase, LeadStore, MAX_RECOVERY_LIMIT, MessageReplay, RecoverableDelivery,
    RecoverableDeliveryState, RegisterLead, SubmitMessage,
};
use reviewq_domain::identity::{DeliveryId, NativeSessionId, RunId, SessionKey, SubmissionId};
use reviewq_domain::session::{
    HostProvider, LeadTarget, NativeBinding, NativeIdentity, SessionTarget,
};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::{identity_for_review, lead_for_run, parse, save_review};
use crate::workspace::{advance_queue, allow_review, configured, provider_label, scope};
use crate::{SqliteStore, StoreError};

pub(crate) mod children;
pub(crate) mod claude;
mod delivery;
pub(crate) use delivery::{admit_workflow, unresolved_workflow_input};
mod messages;
pub(crate) mod metadata;
pub(crate) mod questions;
pub(crate) mod recovery;

impl SqliteStore {
    /// Reopens claims left by the previous exclusive profile owner for read-only reconciliation.
    pub async fn recover_delivery_claims(&self) -> Result<(), StoreError> {
        self.write(|tx| {
            tx.sql.execute("UPDATE managed_interrupts SET state='uncertain' WHERE state='reserved'",[])?;
            let recovered = tx.sql.execute(
                "UPDATE outbox SET due_at_ms=min(due_at_ms, ?1), claim_kind=NULL
                 WHERE claim_kind IS NOT NULL AND delivery_id NOT IN (SELECT delivery_id FROM claude_deliveries WHERE finished_kind IS NULL)",
                [now_ms()?],
            )?;
            tx.delivery_changed = recovered > 0;
            Ok(())
        })
        .await
    }
}

impl LeadStore for SqliteStore {
    fn register_lead(
        &self,
        actor: Actor,
        input: RegisterLead,
    ) -> AppFuture<'_, Snapshot<LeadTarget>> {
        Box::pin(async move {
            self.write(move |tx| register(tx, actor, input))
                .await
                .map_err(StoreError::app)
        })
    }

    fn lookup_lead(
        &self,
        actor: Actor,
        run_id: RunId,
        session_key: SessionKey,
    ) -> AppFuture<'_, Snapshot<LeadTarget>> {
        Box::pin(async move {
            self.read(move |tx| {
                let target = lead_for_run(tx, &run_id)?.ok_or(AppError::NotFound)?;
                if target.session_key() != &session_key {
                    return Err(AppError::NotFound.into());
                }
                allow_target(tx, &actor, &target)?;
                snapshot(tx, target)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn admit_message(
        &self,
        actor: Actor,
        input: SubmitMessage,
    ) -> AppFuture<'_, Snapshot<DeliveryReceipt>> {
        Box::pin(async move {
            self.write(move |tx| delivery::admit(tx, actor, input))
                .await
                .map_err(StoreError::app)
        })
    }

    fn replay_message(
        &self,
        actor: Actor,
        input: MessageReplay,
    ) -> AppFuture<'_, Option<Snapshot<DeliveryReceipt>>> {
        Box::pin(async move {
            self.read(move |db| delivery::replay(db, actor, input))
                .await
                .map_err(StoreError::app)
        })
    }

    fn lookup_native_lead(
        &self,
        binding: NativeBinding,
    ) -> AppFuture<'_, Snapshot<Option<LeadTarget>>> {
        Box::pin(async move {
            self.read(move |tx| {
                let target = native_lead(tx, binding.identity())?;
                if let Some(target) = &target {
                    if target.binding() != &binding {
                        return Err(AppError::Forbidden.into());
                    }
                    allow_target(
                        tx,
                        &Actor::native_session(binding.identity().clone()),
                        target,
                    )?;
                }
                snapshot(tx, target)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_receipt(
        &self,
        actor: Actor,
        target: SessionTarget,
        submission_id: SubmissionId,
    ) -> AppFuture<'_, Snapshot<DeliveryReceipt>> {
        Box::pin(async move {
            self.read(move |tx| delivery::read_receipt(tx, actor, target, submission_id))
                .await
                .map_err(StoreError::app)
        })
    }

    fn recoverable_deliveries(
        &self,
        provider: HostProvider,
        after: Option<DeliveryRecoveryCursor>,
        limit: u16,
    ) -> AppFuture<'_, Vec<RecoverableDelivery>> {
        Box::pin(async move {
            if !(1..=MAX_RECOVERY_LIMIT).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| delivery::recoverable(tx, provider, after, limit))
                .await
                .map_err(StoreError::app)
        })
    }

    fn next_delivery_due(&self, provider: HostProvider) -> AppFuture<'_, Option<i64>> {
        Box::pin(async move {
            self.read(move |tx| delivery::next_due(tx, provider))
                .await
                .map_err(StoreError::app)
        })
    }

    fn claim_delivery(
        &self,
        delivery_id: DeliveryId,
        expected_state: RecoverableDeliveryState,
    ) -> AppFuture<'_, Option<DeliveryClaim>> {
        Box::pin(async move {
            self.write(move |tx| delivery::claim(tx, delivery_id, expected_state))
                .await
                .map_err(StoreError::app)
        })
    }

    fn authorize_dispatch(&self, claim: DeliveryClaim) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.read(move |tx| delivery::authorize_dispatch(tx, claim))
                .await
                .map_err(StoreError::app)
        })
    }

    fn authorize_followup_dispatch(
        &self,
        claim: DeliveryClaim,
        pending_threads: Vec<NativeSessionId>,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.read(move |db| delivery::authorize_followup_dispatch(db, claim, pending_threads))
                .await
                .map_err(StoreError::app)
        })
    }

    fn workflow_requirements(
        &self,
        claim: &DeliveryClaim,
    ) -> AppFuture<'_, reviewq_app::workflow::WorkflowRequirements> {
        let claim = claim.clone();
        Box::pin(async move {
            self.read(move |db| delivery::workflow_requirements(db, claim))
                .await
                .map_err(StoreError::app)
        })
    }

    fn finish_delivery(
        &self,
        claim: DeliveryClaim,
        observation: DeliveryObservation,
    ) -> AppFuture<'_, DeliveryReceipt> {
        Box::pin(async move {
            self.write(move |tx| delivery::finish(tx, claim, observation))
                .await
                .map_err(StoreError::app)
        })
    }
}

pub(crate) fn register(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    input: RegisterLead,
) -> Result<Snapshot<LeadTarget>, StoreError> {
    if crate::workspace::settings_revision(&tx.sql)? != input.expected_settings {
        return Err(AppError::StaleRevision.into());
    }
    let identity = NativeIdentity::new(input.provider, input.profile, input.native_id);
    let binding = NativeBinding::new(identity, input.cwd);
    match actor.scope() {
        ActorScope::LocalOperator => {}
        ActorScope::CheckoutGrant(_) => {
            scope(&tx.sql, &actor)?;
            if !configured(&tx.sql, &input.review.identity)? {
                return Err(AppError::NotFound.into());
            }
        }
        ActorScope::NativeSession(native) => {
            if native != binding.identity() || !configured(&tx.sql, &input.review.identity)? {
                return Err(AppError::Forbidden.into());
            }
        }
    }

    let native = binding.identity();
    let metadata_update = match native.provider() {
        HostProvider::Codex => metadata::MetadataUpdate::Full,
        HostProvider::Claude => metadata::MetadataUpdate::Partial,
    };
    if let Some(target) = native_lead(&tx.sql, native)? {
        if target.binding() != &binding
            || identity_for_review(&tx.sql, target.review_id())? != input.review.identity
        {
            return Err(AppError::Conflict.into());
        }
        if let Some(source) = &input.native_source {
            tx.sql.execute(
                "UPDATE sessions SET source_path=?2 WHERE session_key=?1 AND source_path IS NOT ?2",
                params![target.session_key().as_str(), bytes(source)],
            )?;
        }
        let metadata_changed = metadata::record(
            &tx.sql,
            target.session_key().as_str(),
            &input.metadata,
            metadata_update,
        )?;
        if metadata_changed {
            tx.emit(&[
                ResourceKey::Queue,
                ResourceKey::Run(Some(target.run_id().clone())),
            ])?;
        }
        if let Some(activity) = input.activity {
            let (phase, questions) = activity_columns(Some(activity));
            if record_activity(&tx.sql, target.session_key().as_str(), phase, questions)? {
                advance_queue(&tx.sql)?;
                tx.emit(&[
                    ResourceKey::Queue,
                    ResourceKey::Run(Some(target.run_id().clone())),
                ])?;
            }
        }
        return snapshot(&tx.sql, target);
    }

    let session_key_used = tx
        .sql
        .query_row(
            "SELECT 1 FROM sessions WHERE session_key=?1",
            [input.session_key.as_str()],
            row_exists,
        )
        .optional()?
        .is_some();
    if session_key_used {
        return Err(AppError::Conflict.into());
    }

    let (review_id, _) = save_review(&tx.sql, &input.review, false)?;
    let run_id: RunId = parse(&Uuid::new_v4().to_string())?;
    tx.sql.execute(
        "INSERT INTO runs(run_id, review_id, lead_session_key, started_at_ms) VALUES (?1, ?2, ?3, ?4)",
        params![run_id.as_str(), review_id.as_str(), input.session_key.as_str(), now_ms()?],
    )?;
    let (phase, questions) = activity_columns(input.activity);
    tx.sql.execute(
        "INSERT INTO sessions(session_key, run_id, provider, profile, native_id, cwd, source_path, host_activity, host_questions)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            input.session_key.as_str(),
            run_id.as_str(),
            provider_label(native.provider()),
            bytes(native.profile()),
            native.native_id().as_str(),
            bytes(binding.cwd()),
            input.native_source.as_ref().map(bytes), phase, questions
        ],
    )?;
    metadata::record(
        &tx.sql,
        input.session_key.as_str(),
        &input.metadata,
        metadata_update,
    )?;
    advance_queue(&tx.sql)?;
    tx.emit(&[
        ResourceKey::Queue,
        ResourceKey::Review(Some(review_id.clone())),
        ResourceKey::Run(Some(run_id.clone())),
        ResourceKey::Session(Some(input.session_key.clone())),
    ])?;
    let target = LeadTarget::new(
        SessionTarget::new(review_id, run_id, input.session_key),
        binding,
    );
    snapshot(&tx.sql, target)
}

fn native_lead(
    tx: &Connection,
    identity: &NativeIdentity,
) -> Result<Option<LeadTarget>, StoreError> {
    let existing = tx.query_row(
        "SELECT run_id,parent_session_key FROM sessions WHERE provider=?1 AND profile=?2 AND native_id=?3",
        params![provider_label(identity.provider()), bytes(identity.profile()), identity.native_id().as_str()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    ).optional()?;
    let Some((run, parent)) = existing else {
        return Ok(None);
    };
    if parent.is_some() {
        return Err(AppError::Conflict.into());
    }
    let target = lead_for_run(tx, &parse(&run)?)?.ok_or(AppError::Conflict)?;
    Ok(Some(target))
}

fn bytes(path: &reviewq_domain::session::AbsolutePath) -> &[u8] {
    path.as_path().as_os_str().as_bytes()
}

fn row_exists(_: &rusqlite::Row<'_>) -> rusqlite::Result<()> {
    Ok(())
}

fn allow_target(
    connection: &Connection,
    actor: &Actor,
    target: &LeadTarget,
) -> Result<(), StoreError> {
    let access = scope(connection, actor)?;
    let source = identity_for_review(connection, target.review_id())?;
    allow_review(connection, access, target.review_id(), &source)?;
    if let ActorScope::NativeSession(identity) = actor.scope()
        && identity != target.binding().identity()
    {
        return Err(AppError::NotFound.into());
    }
    Ok(())
}

/// Stores a session's observed activity and returns whether it changed. When the session is
/// its run's lead and becomes working or blocked from anything else, it started a turn: the
/// run's open pass records that its lead started and a held pass reopens. Moving between
/// working and blocked is the same turn continuing. A lead that becomes failed has started the
/// pass too, without reopening a hold.
pub(crate) fn record_activity(
    db: &Connection,
    session: &str,
    phase: Option<&str>,
    questions: bool,
) -> Result<bool, StoreError> {
    let before = db
        .prepare_cached(
            "SELECT s.run_id, s.host_activity, r.lead_session_key IS s.session_key
             FROM sessions s LEFT JOIN runs r ON r.run_id=s.run_id
             WHERE s.session_key=?1 AND (s.host_activity,s.host_questions) IS DISTINCT FROM (?2,?3)",
        )?
        .query_row(params![session, phase, questions], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })
        .optional()?;
    let Some((run, previous, lead)) = before else {
        return Ok(false);
    };
    db.prepare_cached(
        "UPDATE sessions SET host_activity=?2,host_questions=?3 WHERE session_key=?1",
    )?
    .execute(params![session, phase, questions])?;
    if lead {
        let in_turn = |phase: Option<&str>| matches!(phase, Some("working" | "blocked"));
        if in_turn(phase) && !in_turn(previous.as_deref()) {
            crate::ledger::passes::lead_started(db, &run, now_ms()?)?;
        } else if phase == Some("failed") {
            crate::ledger::passes::mark_started(db, &run, now_ms()?)?;
        }
    }
    Ok(true)
}

pub(crate) fn activity_columns(activity: Option<HostActivity>) -> (Option<&'static str>, bool) {
    let Some(value) = activity else {
        return (None, false);
    };
    let phase = match value.phase {
        HostPhase::Idle => "idle",
        HostPhase::Working => "working",
        HostPhase::Blocked => "blocked",
        HostPhase::Failed => "failed",
    };
    (Some(phase), value.has_questions)
}
