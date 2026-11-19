use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::hosts::{HostProof, RegisterLead};
use reviewq_app::launch::{
    LaunchClaim, LaunchInput, LaunchReceipt, LaunchState, LaunchStore, NativeLaunchResult,
    ObservedLaunchSettings,
};
use reviewq_app::queue::{MergeRequestState, ReviewSource};
use reviewq_domain::identity::{DeliveryId, ReviewId, Revision, SubmissionId};
use reviewq_domain::session::{AbsolutePath, HostProvider, LeadTarget};
use rusqlite::{Connection, Row, params};
use uuid::Uuid;

use crate::events::{now_ms, snapshot};
use crate::queue::{identity_for_review, parse, text};
use crate::workspace::{
    actor_key, allow_review, configured, provider_label, scope, settings_revision,
};
use crate::{SqliteStore, StoreError};

const RECEIPT: &str =
    "review_id, submission_id, state, run_id, session_key, native_id, created_at_ms, updated_at_ms";

impl LaunchStore for SqliteStore {
    fn launch_receipt(
        &self,
        actor: Actor,
        review: ReviewId,
        submission: SubmissionId,
        digest: Option<[u8; 32]>,
    ) -> AppFuture<'_, Option<Snapshot<LaunchReceipt>>> {
        Box::pin(async move {
            self.read(move |db| previous(db, &actor, &review, &submission, digest))
                .await
                .map_err(StoreError::app)
        })
    }

    fn claim_launch(
        &self,
        actor: Actor,
        input: LaunchInput,
        submission: SubmissionId,
        digest: [u8; 32],
    ) -> AppFuture<'_, LaunchClaim> {
        Box::pin(async move {
            self.write(move |tx| {
                let db = &tx.sql;
                if let Some(found) = previous(db, &actor, &input.review, &submission, Some(digest))?
                {
                    return Ok(LaunchClaim::Existing(found));
                }
                let identity = identity_for_review(db, &input.review)?;
                authorize(db, &actor, &input.review)?;
                if identity != input.source.identity || !configured(db, &identity)? {
                    return Err(AppError::NotFound.into());
                }
                if settings_revision(db)? != input.settings
                    || input.source.state != MergeRequestState::Open
                {
                    return Err(AppError::StaleRevision.into());
                }
                let id: DeliveryId = parse(&Uuid::new_v4().to_string())?;
                db.execute(
                    "INSERT INTO launch_attempts(id,actor,submission_id,review_id,request_digest,
                        settings_revision,source_sha,provider,checkout,common_dir,requested_model,
                        requested_effort,state,created_at_ms,updated_at_ms)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'pending',?13,?13)",
                    params![
                        id.to_string(),
                        actor_key(&actor).as_slice(),
                        submission.as_str(),
                        input.review.as_str(),
                        digest.as_slice(),
                        input.settings.as_sqlite_integer(),
                        input.source.head_sha.as_str(),
                        provider_label(input.provider),
                        bytes(&input.checkout.root),
                        bytes(&input.checkout.common_dir),
                        input.model,
                        input.effort,
                        now_ms()?
                    ],
                )?;
                check(db, &actor, &id)?;
                tx.emit(&[ResourceKey::Review(Some(input.review))])?;
                Ok(LaunchClaim::Admitted(id))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn check_launch(&self, actor: Actor, id: DeliveryId) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.read(move |db| check(db, &actor, &id).map(|_| ()))
                .await
                .map_err(StoreError::app)
        })
    }

    fn register_launch(
        &self,
        actor: Actor,
        id: DeliveryId,
        proof: HostProof,
        settings: Option<ObservedLaunchSettings>,
    ) -> AppFuture<'_, LeadTarget> {
        Box::pin(async move {
            let native = proof.binding.identity().clone();
            let checkout = proof.binding.cwd().clone();
            let locator = id.clone();
            let issuer = actor_key(&actor);
            self.write(move |tx| {
                let recorded = tx.sql.execute(
                    "UPDATE launch_attempts SET profile=?2,native_id=?3,observed_model=?4,observed_effort=?5,
                       updated_at_ms=max(updated_at_ms,?6)
                     WHERE id=?1 AND state='pending' AND actor=?7 AND provider=?8 AND checkout=?9
                       AND (native_id IS NULL OR (native_id=?3 AND profile=?2))",
                    params![
                        locator.to_string(), bytes(native.profile()), native.native_id().as_str(),
                        settings.as_ref().map(|settings| &settings.model),
                        settings.as_ref().and_then(|settings| settings.effort.as_deref()), now_ms()?,
                        issuer.as_slice(), provider_label(native.provider()), bytes(&checkout),
                    ],
                )?;
                if recorded != 1 {
                    return Err(AppError::Conflict.into());
                }
                let review: String = tx.sql.query_row(
                    "SELECT review_id FROM launch_attempts WHERE id=?1", [locator.to_string()], |row| row.get(0)
                )?;
                tx.emit(&[ResourceKey::Review(Some(parse(&review)?))])?;
                Ok(())
            }).await.map_err(StoreError::app)?;
            let native = proof.binding.identity().clone();
            self.write(move |tx| {
                let bound = check(&tx.sql, &actor, &id)?;
                let found = tx.sql.query_row(
                    "SELECT run_id FROM launch_attempts WHERE id=?1",
                    [id.to_string()], |row| row.get::<_, Option<String>>(0),
                )?;
                if let Some(run) = found {
                    return crate::queue::lead_for_run(&tx.sql, &parse(&run)?)?
                        .filter(|target| target.binding() == &proof.binding)
                        .ok_or_else(|| AppError::Conflict.into());
                }
                let source = source(&tx.sql, &bound.review)?;
                let session_key = match native.provider() {
                    HostProvider::Codex => parse(native.native_id().as_str())?,
                    HostProvider::Claude => parse(&format!("claude:{}", native.native_id()))?,
                };
                let registered = crate::hosts::register(tx, actor, RegisterLead {
                    expected_settings: bound.settings,
                    review: source,
                    provider: native.provider(),
                    profile: native.profile().clone(),
                    native_id: native.native_id().clone(),
                    session_key,
                    cwd: proof.binding.cwd().clone(),
                    native_source: proof.source,
                    activity: proof.activity,
                })?.value;
                if registered.review_id() != &bound.review {
                    return Err(AppError::Conflict.into());
                }
                tx.sql.execute(
                    "UPDATE launch_attempts SET run_id=?2,session_key=?3,updated_at_ms=max(updated_at_ms,?4)
                     WHERE id=?1 AND state='pending'",
                    params![id.to_string(), registered.run_id().as_str(), registered.session_key().as_str(), now_ms()?],
                )?;
                Ok(registered)
            }).await.map_err(StoreError::app)
        })
    }

    fn finish_launch(
        &self,
        id: DeliveryId,
        state: NativeLaunchResult,
    ) -> AppFuture<'_, Snapshot<LaunchReceipt>> {
        Box::pin(async move {
            self.write(move |tx| {
                let state = match state {
                    NativeLaunchResult::Confirmed => "confirmed",
                    NativeLaunchResult::Rejected => "rejected",
                    NativeLaunchResult::Uncertain => "uncertain",
                };
                let changed = tx.sql.execute(
                    "UPDATE launch_attempts SET state=CASE ?2
                        WHEN 'confirmed' THEN CASE WHEN run_id IS NULL THEN 'uncertain' ELSE 'confirmed' END
                        WHEN 'rejected' THEN CASE WHEN native_id IS NULL THEN 'rejected' ELSE 'uncertain' END
                        ELSE 'uncertain' END,
                        updated_at_ms=max(updated_at_ms,?3)
                     WHERE id=?1 AND state='pending'",
                    params![id.to_string(), state, now_ms()?],
                )?;
                let query = format!("SELECT {RECEIPT} FROM launch_attempts WHERE id=?1");
                let mut query = tx.sql.prepare_cached(&query)?;
                let mut rows = query.query([id.to_string()])?;
                let value = receipt(rows.next()?.ok_or(AppError::NotFound)?)?;
                drop(rows);
                drop(query);
                if changed != 0 { tx.emit(&[ResourceKey::Review(Some(value.review.clone()))])?; }
                snapshot(&tx.sql, value)
            }).await.map_err(StoreError::app)
        })
    }

    fn recover_launches(&self) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(|tx| {
                if tx.sql.execute(
                    "UPDATE launch_attempts SET state='uncertain',updated_at_ms=max(updated_at_ms,?1)
                     WHERE state='pending'", [now_ms()?]
                )? != 0 {
                    tx.emit(&[ResourceKey::Review(None)])?;
                }
                Ok(())
            }).await.map_err(StoreError::app)
        })
    }
}

struct Bound {
    review: ReviewId,
    settings: Revision,
}

fn check(db: &Connection, actor: &Actor, id: &DeliveryId) -> Result<Bound, StoreError> {
    let mut query = db.prepare_cached(
        "SELECT a.review_id,a.settings_revision FROM launch_attempts a
         JOIN reviews r USING(review_id)
         JOIN settings_repositories b ON b.project_id=r.project_id AND b.checkout=a.checkout AND b.common_dir=a.common_dir
         JOIN configured_projects p ON p.origin=r.origin AND p.project_id=r.project_id
         WHERE a.id=?1 AND a.actor=?2 AND a.state='pending' AND r.state='open' AND r.head_sha=a.source_sha",
    )?;
    let mut rows = query.query(params![id.to_string(), actor_key(actor).as_slice()])?;
    let row = rows.next()?.ok_or(AppError::StaleRevision)?;
    let review = parse(text(row, 0)?)?;
    authorize(db, actor, &review)?;
    let settings = Revision::try_from(row.get::<_, i64>(1)?).map_err(|_| AppError::Internal)?;
    if settings_revision(db)? != settings {
        return Err(AppError::StaleRevision.into());
    }
    Ok(Bound { review, settings })
}

fn previous(
    db: &Connection,
    actor: &Actor,
    review: &ReviewId,
    submission: &SubmissionId,
    digest: Option<[u8; 32]>,
) -> Result<Option<Snapshot<LaunchReceipt>>, StoreError> {
    authorize(db, actor, review)?;
    let query = format!(
        "SELECT {RECEIPT}, request_digest FROM launch_attempts WHERE actor=?1 AND submission_id=?2"
    );
    let mut query = db.prepare_cached(&query)?;
    let mut rows = query.query(params![actor_key(actor).as_slice(), submission.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let value = receipt(row)?;
    let saved: Vec<u8> = row.get(8)?;
    if &value.review != review || digest.is_some_and(|digest| saved != digest) {
        return Err(if digest.is_some() {
            AppError::Conflict
        } else {
            AppError::NotFound
        }
        .into());
    }
    Ok(Some(snapshot(db, value)?))
}

fn authorize(db: &Connection, actor: &Actor, review: &ReviewId) -> Result<(), StoreError> {
    if matches!(actor.scope(), ActorScope::NativeSession(_)) {
        return Err(AppError::Forbidden.into());
    }
    let identity = identity_for_review(db, review)?;
    allow_review(db, scope(db, actor)?, review, &identity)
}

fn receipt(row: &Row<'_>) -> Result<LaunchReceipt, StoreError> {
    let optional = |column| row.get::<_, Option<String>>(column);
    Ok(LaunchReceipt {
        review: parse(text(row, 0)?)?,
        submission: parse(text(row, 1)?)?,
        state: match text(row, 2)? {
            "pending" => LaunchState::Pending,
            "confirmed" => LaunchState::Confirmed,
            "uncertain" => LaunchState::Uncertain,
            "rejected" => LaunchState::Rejected,
            _ => return Err(AppError::Internal.into()),
        },
        run: optional(3)?.map(|value| parse(&value)).transpose()?,
        session: optional(4)?.map(|value| parse(&value)).transpose()?,
        native: optional(5)?.map(|value| parse(&value)).transpose()?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn source(db: &Connection, review: &ReviewId) -> Result<ReviewSource, StoreError> {
    let (project_path, title, sha, updated_at_ms) = db.query_row(
        "SELECT project_path,title,head_sha,updated_at_ms FROM reviews WHERE review_id=?1",
        [review.as_str()],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, String>(2)?,
                row.get(3)?,
            ))
        },
    )?;
    Ok(ReviewSource {
        identity: identity_for_review(db, review)?,
        project_path,
        title,
        head_sha: parse(&sha)?,
        state: MergeRequestState::Open,
        updated_at_ms,
    })
}

fn bytes(path: &AbsolutePath) -> &[u8] {
    path.as_path().as_os_str().as_bytes()
}
