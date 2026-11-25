use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::hosts::{HostProof, ObservedInput, RegisterLead};
use reviewq_app::launch::{
    ClaudeLaunchLocator, Invocation, LaunchClaim, LaunchDetails, LaunchInput, LaunchReceipt,
    LaunchState, LaunchStore, NativeLaunchResult, ObservedLaunchSettings,
};
use reviewq_app::queue::MergeRequestState;
use reviewq_domain::identity::{
    DeliveryId, NativeSessionId, ReviewId, Revision, RunId, SubmissionId,
};
use reviewq_domain::session::{AbsolutePath, HostProvider, LeadTarget};
use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::{
    identity_for_review, lead_for_run, parse, path, save_observations, source_for_review, text,
};
use crate::workspace::{
    actor_key, allow_review, configured, provider_label, scope, settings_revision,
};
use crate::{SqliteStore, StoreError};

const RECEIPT: &str =
    "review_id, submission_id, state, run_id, session_key, native_id, created_at_ms, updated_at_ms,
     id, provider, native_job_id, native_turn_id, native_entry_id, profile, checkout,
     CASE WHEN provider='claude' THEN json_extract(invocation,'$.claude') END AS claude_locator";

#[derive(Deserialize, Serialize)]
struct StoredSelection {
    #[serde(flatten)]
    invocation: StoredInvocation,
    context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude: Option<StoredClaudeLocator>,
}

#[derive(Deserialize, Serialize)]
struct StoredClaudeLocator {
    profile: Vec<u8>,
    name: String,
    job: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredInvocation {
    Custom {
        prompt: String,
    },
    Skill {
        name: String,
        path: std::path::PathBuf,
        args: Option<String>,
    },
}

impl LaunchStore for SqliteStore {
    fn launch_receipt(
        &self,
        actor: Actor,
        review: Option<ReviewId>,
        submission: SubmissionId,
        digest: Option<[u8; 32]>,
    ) -> AppFuture<'_, Option<Snapshot<LaunchReceipt>>> {
        Box::pin(async move {
            self.read(move |db| previous(db, &actor, review.as_ref(), &submission, digest))
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
                if let Some(found) =
                    previous(db, &actor, Some(&input.review), &submission, Some(digest))?
                {
                    return Ok(LaunchClaim::Existing(Box::new(found)));
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
                let current = source_for_review(db, &input.review)?;
                if current != input.source && current != input.prior_source {
                    return Err(AppError::StaleRevision.into());
                }
                if input.source.updated_at_ms < current.updated_at_ms
                    && input.source.head_sha != current.head_sha
                {
                    return Err(AppError::StaleRevision.into());
                }
                save_observations(tx, std::slice::from_ref(&input.source))?;
                let db = &tx.sql;
                let id: DeliveryId = parse(&Uuid::new_v4().to_string())?;
                let created_at_ms = now_ms()?;
                db.execute(
                    "INSERT INTO launch_attempts(id,actor,submission_id,review_id,request_digest,
                        settings_revision,source_sha,provider,checkout,common_dir,requested_model,
                        requested_effort,invocation,state,created_at_ms,updated_at_ms)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'pending',?14,?14)",
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
                        invocation(&input)?,
                        created_at_ms,
                    ],
                )?;
                check(db, &actor, &id)?;
                tx.emit(&[ResourceKey::Review(Some(input.review))])?;
                Ok(LaunchClaim::Admitted { id, created_at_ms })
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

    fn record_claude_locator(
        &self,
        actor: Actor,
        id: DeliveryId,
        profile: AbsolutePath,
        name: String,
        job: Option<String>,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            if let Some(job) = &job {
                validate_job(job)?;
            }
            let locator = serde_json::to_string(&StoredClaudeLocator {
                profile: bytes(&profile).to_vec(),
                name,
                job,
            })
            .map_err(|_| AppError::Internal)?;
            self.write(move |tx| {
                // Keep the private CLI parameters with the existing immutable request.
                // The native identity columns remain empty until roster readback.
                let changed = tx.sql.execute(
                    "UPDATE launch_attempts SET invocation=json_set(invocation,'$.claude',json(?3))
                     WHERE id=?1 AND actor=?2 AND provider='claude' AND state='pending'
                       AND (json_extract(invocation,'$.claude') IS NULL OR (
                         json_extract(invocation,'$.claude.profile')=json_extract(?3,'$.profile')
                         AND json_extract(invocation,'$.claude.name')=json_extract(?3,'$.name')
                         AND (json_extract(invocation,'$.claude.job') IS NULL OR
                           json_extract(invocation,'$.claude.job')=json_extract(?3,'$.job'))))",
                    params![id.as_str(), actor_key(&actor).as_slice(), locator],
                )?;
                if changed != 1 {
                    return Err(AppError::Conflict.into());
                }
                Ok(())
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn record_launch_native(
        &self,
        actor: Actor,
        id: DeliveryId,
        proof: HostProof,
        job: Option<String>,
        settings: Option<ObservedLaunchSettings>,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            if let Some(job) = &job {
                validate_job(job)?;
            }
            let binding = proof.binding;
            let issuer = actor_key(&actor);
            self.write(move |tx| {
                let native = binding.identity();
                let recorded = tx.sql.execute(
                    "UPDATE launch_attempts SET profile=?2,native_id=?3,
                       observed_model=coalesce(?4,observed_model),observed_effort=coalesce(?5,observed_effort),
                       native_job_id=coalesce(?10,native_job_id),
                       updated_at_ms=max(updated_at_ms,?6)
                     WHERE id=?1 AND state='pending' AND actor=?7 AND provider=?8 AND checkout=?9
                       AND (native_id IS NULL OR (native_id=?3 AND profile=?2))
                       AND (?10 IS NULL OR native_job_id IS NULL OR native_job_id=?10)",
                    params![
                        id.as_str(), bytes(native.profile()), native.native_id().as_str(),
                        settings.as_ref().map(|settings| &settings.model),
                        settings.as_ref().and_then(|settings| settings.effort.as_deref()), now_ms()?,
                        issuer.as_slice(), provider_label(native.provider()), bytes(binding.cwd()),
                        job,
                    ],
                )?;
                if recorded != 1 {
                    return Err(AppError::Conflict.into());
                }
                let review: String = tx.sql.query_row(
                    "SELECT review_id FROM launch_attempts WHERE id=?1", [id.as_str()], |row| row.get(0)
                )?;
                tx.emit(&[ResourceKey::Review(Some(parse(&review)?))])?;
                Ok(())
            }).await.map_err(StoreError::app)
        })
    }

    fn record_launch_input(&self, id: DeliveryId, input: ObservedInput) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                let affected = tx.sql.execute(
                    "UPDATE launch_attempts SET native_turn_id=?2,native_entry_id=?3,updated_at_ms=max(updated_at_ms,?4)
                     WHERE id=?1 AND provider='codex' AND run_id IS NOT NULL
                       AND state IN ('pending','uncertain') AND (native_turn_id IS NULL OR
                         (native_turn_id=?2 AND native_entry_id=?3))",
                    params![id.to_string(),input.turn_id.as_str(),input.entry_id.as_str(),now_ms()?],
                )?;
                if affected != 1 { return Err(AppError::Conflict.into()); }
                Ok(())
            }).await.map_err(StoreError::app)
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
            self.record_launch_native(actor.clone(), id.clone(), proof.clone(), None, settings)
                .await?;
            self.write(move |tx| {
                let bound = check(&tx.sql, &actor, &id)?;
                register_bound(tx, actor, &id, bound, proof)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn confirm_claude_launch(
        &self,
        actor: Actor,
        review: ReviewId,
        submission: SubmissionId,
        proof: HostProof,
        job: String,
    ) -> AppFuture<'_, Snapshot<LaunchReceipt>> {
        Box::pin(async move {
            validate_job(&job)?;
            self.write(move |tx| {
                let stored = previous(&tx.sql, &actor, Some(&review), &submission, None)?
                    .ok_or(AppError::NotFound)?;
                let value = stored.value;
                let native = proof.binding.identity().clone();
                let locator = value.claude.as_ref().ok_or(AppError::Conflict)?;
                if native.provider() != HostProvider::Claude
                    || native.profile() != &locator.profile
                    || proof.binding.cwd() != &locator.checkout
                    || locator.native.as_ref().is_some_and(|expected| expected != native.native_id())
                    || locator.job.as_ref().is_some_and(|expected| expected != &job)
                {
                    return Err(AppError::Conflict.into());
                }
                if value.state != LaunchState::Uncertain {
                    return Ok(Snapshot { cursor: stored.cursor, value });
                }
                let claimed: bool = tx.sql.query_row(
                    "SELECT EXISTS(SELECT 1 FROM launch_attempts WHERE id<>?1 AND provider='claude' AND profile=?2 AND native_id=?3)",
                    params![value.id.as_str(), bytes(native.profile()), native.native_id().as_str()],
                    |row| row.get(0),
                )?;
                if claimed {
                    return Err(AppError::Conflict.into());
                }
                // The provider already created this job. Reconcile the old attempt even
                // if its original MR head or settings changed after launch.
                let settings = settings_revision(&tx.sql)?;
                register_bound(tx, actor, &value.id, Bound { review: review.clone(), settings }, proof)?;
                let time = now_ms()?;
                tx.sql.execute(
                    "UPDATE launch_attempts SET state='confirmed',profile=?2,native_id=?3,native_job_id=?4,
                      updated_at_ms=max(updated_at_ms,?5) WHERE id=?1 AND state='uncertain'",
                    params![value.id.as_str(), bytes(native.profile()), native.native_id().as_str(), job, time],
                )?;
                crate::ledger::passes::open_for_launch(&tx.sql, value.id.as_str(), time)?;
                tx.emit(&[ResourceKey::Review(Some(review))])?;
                snapshot(&tx.sql, receipt_by_id(&tx.sql, &value.id)?)
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
                let time = now_ms()?;
                let changed = tx.sql.execute(
                    "UPDATE launch_attempts SET state=CASE ?2
                        WHEN 'confirmed' THEN CASE WHEN run_id IS NULL OR (provider='codex' AND native_turn_id IS NULL)
                           THEN 'uncertain' ELSE 'confirmed' END
                        WHEN 'rejected' THEN CASE WHEN native_id IS NULL THEN 'rejected' ELSE 'uncertain' END
                        ELSE 'uncertain' END,
                        updated_at_ms=max(updated_at_ms,?3)
                     WHERE id=?1 AND (state='pending' OR (state='uncertain' AND ?2='confirmed'))",
                    params![id.to_string(), state, time],
                )?;
                let value = receipt_by_id(&tx.sql, &id)?;
                if changed != 0 {
                    crate::ledger::passes::open_for_launch(&tx.sql, id.as_str(), time)?;
                    tx.emit(&[ResourceKey::Review(Some(value.review.clone()))])?;
                }
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

fn register_bound(
    tx: &mut WriteTx<'_>,
    actor: Actor,
    id: &DeliveryId,
    bound: Bound,
    proof: HostProof,
) -> Result<LeadTarget, StoreError> {
    let found = tx.sql.query_row(
        "SELECT run_id FROM launch_attempts WHERE id=?1",
        [id.as_str()],
        |row| row.get::<_, Option<String>>(0),
    )?;
    if let Some(run) = found {
        return lead_for_run(&tx.sql, &parse(&run)?)?
            .filter(|target| {
                target.binding() == &proof.binding && target.review_id() == &bound.review
            })
            .ok_or_else(|| AppError::Conflict.into());
    }
    let native = proof.binding.identity();
    let source = source_for_review(&tx.sql, &bound.review)?;
    let session_key = match native.provider() {
        HostProvider::Codex => parse(native.native_id().as_str())?,
        HostProvider::Claude => parse(&format!("claude:{}", native.native_id()))?,
    };
    let registered = crate::hosts::register(
        tx,
        actor,
        RegisterLead {
            expected_settings: bound.settings,
            review: source,
            provider: native.provider(),
            profile: native.profile().clone(),
            native_id: native.native_id().clone(),
            session_key,
            cwd: proof.binding.cwd().clone(),
            native_source: proof.source,
            activity: proof.activity,
            metadata: proof.metadata,
        },
    )?
    .value;
    if registered.review_id() != &bound.review {
        return Err(AppError::Conflict.into());
    }
    let changed = tx.sql.execute(
        "UPDATE launch_attempts SET run_id=?2,session_key=?3,updated_at_ms=max(updated_at_ms,?4)
         WHERE id=?1 AND state IN ('pending','uncertain')",
        params![
            id.as_str(),
            registered.run_id().as_str(),
            registered.session_key().as_str(),
            now_ms()?
        ],
    )?;
    if changed != 1 {
        return Err(AppError::Conflict.into());
    }
    Ok(registered)
}

fn validate_job(job: &str) -> Result<(), AppError> {
    if job.is_empty()
        || job.len() > 256
        || !job
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::InvalidRequest);
    }
    Ok(())
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
    review: Option<&ReviewId>,
    submission: &SubmissionId,
    digest: Option<[u8; 32]>,
) -> Result<Option<Snapshot<LaunchReceipt>>, StoreError> {
    if let Some(review) = review {
        authorize(db, actor, review)?;
    }
    let query = format!(
        "SELECT {RECEIPT}, request_digest FROM launch_attempts WHERE actor=?1 AND submission_id=?2"
    );
    let mut query = db.prepare_cached(&query)?;
    let mut rows = query.query(params![actor_key(actor).as_slice(), submission.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let value = receipt(row)?;
    let saved: Vec<u8> = row.get("request_digest")?;
    authorize(db, actor, &value.review)?;
    if review.is_some_and(|review| review != &value.review)
        || digest.is_some_and(|digest| saved != digest)
    {
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
    let provider = crate::queue::provider(text(row, 9)?)?;
    let native: Option<NativeSessionId> = optional(5)?.map(|value| parse(&value)).transpose()?;
    let job = optional(10)?;
    let claude = if provider == HostProvider::Claude {
        let encoded: Option<String> = row.get("claude_locator")?;
        let stored: Option<StoredClaudeLocator> = encoded
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| AppError::Internal)?;
        let profile: Option<Vec<u8>> = row.get("profile")?;
        let (profile, name, acknowledged) = match stored {
            Some(stored) => (Some(stored.profile), Some(stored.name), stored.job),
            None => (profile, None, None),
        };
        profile
            .map(|profile| {
                Ok::<_, StoreError>(ClaudeLaunchLocator {
                    profile: path(profile)?,
                    checkout: path(row.get("checkout")?)?,
                    name,
                    native: native.clone(),
                    job: job.clone().or(acknowledged),
                })
            })
            .transpose()?
    } else {
        None
    };
    let job = job.or_else(|| claude.as_ref().and_then(|locator| locator.job.clone()));
    Ok(LaunchReceipt {
        id: parse(text(row, 8)?)?,
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
        native,
        provider,
        job,
        claude,
        observed_input: match (optional(11)?, optional(12)?) {
            (Some(turn), Some(entry)) => Some(ObservedInput {
                turn_id: parse(&turn)?,
                entry_id: parse(&entry)?,
            }),
            (None, None) => None,
            _ => return Err(AppError::Internal.into()),
        },
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn receipt_by_id(db: &Connection, id: &DeliveryId) -> Result<LaunchReceipt, StoreError> {
    let query = format!("SELECT {RECEIPT} FROM launch_attempts WHERE id=?1");
    let mut query = db.prepare_cached(&query)?;
    let mut rows = query.query([id.as_str()])?;
    receipt(rows.next()?.ok_or(AppError::NotFound)?)
}

fn bytes(path: &AbsolutePath) -> &[u8] {
    path.as_path().as_os_str().as_bytes()
}

fn invocation(input: &LaunchInput) -> Result<String, StoreError> {
    let invocation = match &input.invocation {
        Invocation::Custom { prompt } => StoredInvocation::Custom {
            prompt: prompt.clone(),
        },
        Invocation::Skill { skill, args } => StoredInvocation::Skill {
            path: skill.path.as_path().to_owned(),
            name: skill.name.clone(),
            args: args.clone(),
        },
    };
    let encoded = serde_json::to_string(&StoredSelection {
        invocation,
        context: input.context.clone(),
        claude: None,
    })
    .map_err(|_| AppError::Internal)?;
    if encoded.len() > 153600 {
        return Err(AppError::PayloadTooLarge.into());
    }
    Ok(encoded)
}

pub(crate) fn request_evidence(
    db: &Connection,
    run: &RunId,
) -> Result<Option<reviewq_app::launch::RequestEvidence>, StoreError> {
    use reviewq_app::launch::{RequestEvidence, RequestInput, RequestInputSlot};
    let mut query = db.prepare_cached(
        "SELECT requested_model, requested_effort, skill_name, input_slot,
                native_turn_id, native_entry_id, source_generation
         FROM run_request_evidence WHERE run_id=?1",
    )?;
    let mut rows = query.query([run.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let input = match row.get::<_, Option<String>>(3)? {
        Some(slot) => Some(RequestInput {
            slot: match slot.as_str() {
                "review_request" => RequestInputSlot::ReviewRequest,
                "skill_arguments" => RequestInputSlot::SkillArguments,
                _ => return Err(AppError::Internal.into()),
            },
            turn: parse(text(row, 4)?)?,
            entry: parse(text(row, 5)?)?,
            generation: row.get(6)?,
        }),
        None => None,
    };
    Ok(Some(RequestEvidence {
        requested_model: row.get(0)?,
        requested_effort: row.get(1)?,
        skill_name: row.get(2)?,
        input,
    }))
}

pub(crate) fn run_launch_details(
    db: &Connection,
    run: &RunId,
) -> Result<Option<LaunchDetails>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT source_sha,state,invocation,requested_model,requested_effort,observed_model,observed_effort
         FROM launch_attempts WHERE run_id=?1"
    )?;
    let mut rows = statement.query([run.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let selection: StoredSelection =
        serde_json::from_str(text(row, 2)?).map_err(|_| AppError::Internal)?;
    let value = LaunchDetails {
        source: parse(text(row, 0)?)?,
        state: match text(row, 1)? {
            "pending" => LaunchState::Pending,
            "confirmed" => LaunchState::Confirmed,
            "rejected" => LaunchState::Rejected,
            "uncertain" => LaunchState::Uncertain,
            _ => return Err(AppError::Internal.into()),
        },
        invocation: match selection.invocation {
            StoredInvocation::Custom { prompt } => Invocation::Custom { prompt },
            StoredInvocation::Skill { path, name, args } => Invocation::Skill {
                skill: reviewq_domain::settings::NativeSkill {
                    name,
                    path: AbsolutePath::from_canonical(path).map_err(|_| AppError::Internal)?,
                },
                args,
            },
        },
        context: selection.context,
        requested_model: row.get(3)?,
        requested_effort: row.get(4)?,
        observed_model: row.get(5)?,
        observed_effort: row.get(6)?,
    };
    Ok(Some(value))
}
