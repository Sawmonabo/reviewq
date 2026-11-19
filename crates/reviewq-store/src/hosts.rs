//! Exact lead registration, scoped admissions, and fenced delivery outcomes.

use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::hosts::{
    DeliveryClaim, DeliveryObservation, DeliveryReceipt, DeliveryRecoveryCursor, LeadStore,
    MAX_RECOVERY_LIMIT, RecoverableDelivery, RecoverableDeliveryState, RegisterLead, SubmitMessage,
};
use reviewq_domain::identity::{DeliveryId, RunId, SessionKey, SubmissionId};
use reviewq_domain::session::{LeadTarget, NativeBinding, NativeIdentity, SessionTarget};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::{identity_for_review, lead_for_run, parse, save_review};
use crate::workspace::{advance_queue, allow_review, configured, provider_label, scope};
use crate::{SqliteStore, StoreError};

mod delivery;

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
        after: Option<DeliveryRecoveryCursor>,
        limit: u16,
    ) -> AppFuture<'_, Vec<RecoverableDelivery>> {
        Box::pin(async move {
            if !(1..=MAX_RECOVERY_LIMIT).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| delivery::recoverable(tx, after, limit))
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

fn register(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    input: RegisterLead,
) -> Result<Snapshot<LeadTarget>, StoreError> {
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
    let existing = tx
        .sql
        .query_row(
            "SELECT run_id FROM sessions WHERE provider=?1 AND profile=?2 AND native_id=?3",
            params![
                provider_label(native.provider()),
                bytes(native.profile()),
                native.native_id().as_str()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(run) = existing {
        let target = lead_for_run(&tx.sql, &parse(&run)?)?.ok_or(AppError::Conflict)?;
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
    tx.sql.execute(
        "INSERT INTO sessions(session_key, run_id, provider, profile, native_id, cwd, source_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            input.session_key.as_str(),
            run_id.as_str(),
            provider_label(native.provider()),
            bytes(native.profile()),
            native.native_id().as_str(),
            bytes(binding.cwd()),
            input.native_source.as_ref().map(bytes)
        ],
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
