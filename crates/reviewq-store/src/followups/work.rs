use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::events::ResourceKey;
use reviewq_app::followups::{
    CheckMember, FollowupWorkerStore, MAX_FROZEN_OBSERVATIONS, MonitoredReview, PendingCheck,
    PreparedCheck,
};
use reviewq_app::reports::ReportReference;
use reviewq_app::workflow::{SelectedNote, WorkflowAdmission, WorkflowBinding};
use reviewq_domain::identity::{ReviewId, RunId, SubmissionId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{preferences, revision};
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::hosts::admit_workflow;
use crate::queue::{identity_for_review, lead_for_run, parse, text};
use crate::workspace::{advance_queue, settings_revision};
use crate::{SqliteStore, StoreError};

const ELIGIBLE: &str = "
  b.state='pending' AND b.authority_verified=1 AND m.binding_verified=1
  AND m.run_id=b.run_id AND m.generation=b.generation AND r.state='open' AND m.reason IS NULL
  AND (b.manual_requested=1 AND m.state IN ('enabled','paused') OR
       m.state='enabled' AND coalesce(m.policy_override,(SELECT automatic FROM followup_preferences WHERE singleton=1),0)=1)
  AND NOT EXISTS(SELECT 1 FROM followup_batches q WHERE q.review_id=b.review_id AND q.authority_verified=1
                 AND q.state IN ('reserved','executing'))
  AND EXISTS(SELECT 1 FROM configured_projects p WHERE p.origin=r.origin AND p.project_id=r.project_id)
  AND NOT EXISTS(SELECT 1 FROM messages s WHERE s.run_id=b.run_id AND s.session_key=b.lead_session_key
     AND s.actor_key IS NOT NULL AND s.state IN ('queued','dispatching','uncertain'))";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FrozenManifest {
    pub(crate) batch_id: String,
    pub(crate) submission_id: String,
    pub(crate) generation: u64,
    pub(crate) source_sha: String,
    pub(crate) target_sha: Option<String>,
    pub(crate) report_receipt_id: Option<String>,
    pub(crate) report_hash: Option<String>,
    pub(crate) disposition_version: u64,
    pub(crate) model: String,
    pub(crate) effort: String,
    pub(crate) observations: Vec<FrozenMember>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FrozenMember {
    pub(crate) id: String,
    pub(crate) discussion_id: String,
    pub(crate) discussion_version: String,
    pub(crate) originals: Vec<FrozenOriginal>,
    pub(crate) finding_ids: Vec<String>,
    pub(crate) prior_batch_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FrozenOriginal {
    pub(crate) note_id: u64,
    pub(crate) reviewer_id: u64,
}

impl FollowupWorkerStore for SqliteStore {
    fn due_checks(&self, limit: u16) -> AppFuture<'_, Vec<PendingCheck>> {
        Box::pin(async move {
            if !(1..=16).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |db| due(db, limit))
                .await
                .map_err(StoreError::app)
        })
    }

    fn hold_check(&self, check: PendingCheck, reason: &'static str) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                let current: Option<String> = tx.sql.query_row(
                    "SELECT hold_reason FROM followup_batches WHERE batch_id=?1 AND review_id=?2
                     AND generation=?3 AND state='pending' AND authority_verified=1",
                    params![check.batch_id,check.monitor.review.as_str(),check.monitor.generation.as_sqlite_integer()],
                    |row| row.get(0),
                ).optional()?.ok_or(AppError::StaleRevision)?;
                tx.sql.execute(
                    "UPDATE followup_batches SET hold_reason=?2,due_at_ms=?3 WHERE batch_id=?1",
                    params![check.batch_id,reason,now_ms()?.saturating_add(10_000)],
                )?;
                if current.as_deref() != Some(reason) {
                    emit(tx, &check.monitor.review, &check.monitor.run)?;
                }
                Ok(())
            }).await.map_err(StoreError::app)
        })
    }

    fn reserve_check(&self, check: PreparedCheck) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| reserve(tx, check))
                .await
                .map_err(StoreError::app)
        })
    }
}

fn due(db: &Connection, limit: u16) -> Result<Vec<PendingCheck>, StoreError> {
    let slots = free_slots(db)?;
    if slots == 0 {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT b.batch_id,b.review_id,b.run_id,b.generation,b.lead_session_key,m.reviewer_id
         FROM followup_batches b JOIN followup_monitors m USING(review_id) JOIN reviews r USING(review_id)
         WHERE b.due_at_ms<=?1 AND {ELIGIBLE}
         ORDER BY b.due_at_ms,b.batch_id LIMIT ?2");
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query(params![now_ms()?, u16::from(slots).min(limit)])?;
    let settings = settings_revision(db)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let batch_id = text(row, 0)?.to_owned();
        let review = parse(text(row, 1)?)?;
        let run = parse(text(row, 2)?)?;
        let lead = lead_for_run(db, &run)?.ok_or(AppError::NotFound)?;
        if lead.review_id() != &review || lead.session_key().as_str() != text(row, 4)? {
            continue;
        }
        let (base, disposition_version) = baseline(db, &run)?;
        let members = members(db, &review, &batch_id)?;
        if members.is_empty() {
            continue;
        }
        let authenticated_reviewer = text(row, 5)?.parse().map_err(|_| AppError::Internal)?;
        result.push(PendingCheck {
            batch_id,
            lead,
            authenticated_reviewer,
            configured_reviewers: super::observations::monitored_reviewers(
                db,
                authenticated_reviewer,
            )?,
            monitor: MonitoredReview {
                identity: identity_for_review(db, &review)?,
                review,
                run,
                generation: revision(row.get(3)?)?,
                settings,
            },
            members,
            base,
            disposition_version,
        });
    }
    Ok(result)
}

fn baseline(
    db: &Connection,
    run: &RunId,
) -> Result<(Option<ReportReference>, reviewq_domain::identity::Revision), StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT h.receipt_id,a.sha256,h.disposition_version FROM report_heads h
         JOIN report_versions v USING(run_id,receipt_id) JOIN artifacts a USING(artifact_id)
         WHERE h.run_id=?1 AND a.sealed=1 AND a.complete=1",
    )?;
    let mut rows = statement.query([run.as_str()])?;
    if let Some(row) = rows.next()? {
        return Ok((
            Some(ReportReference {
                receipt_id: parse(text(row, 0)?)?,
                hash: parse(text(row, 1)?)?,
            }),
            revision(row.get(2)?)?,
        ));
    }
    Ok((None, revision(0)?))
}

fn members(
    db: &Connection,
    review: &ReviewId,
    batch: &str,
) -> Result<Vec<CheckMember>, StoreError> {
    let mut statement = db.prepare_cached(
           "SELECT o.observation_id,o.discussion_id,o.discussion_version,o.original_note_id,m.reconciles_batch_id,
          (SELECT json_group_array(f.finding_id) FROM (
             SELECT DISTINCT finding_id FROM followup_thread_findings
             WHERE review_id=o.review_id AND generation=o.generation AND (
                 original_note_id=o.original_note_id OR original_note_id IN (
                     SELECT original_note_id FROM followup_observation_origins l
                      WHERE l.review_id=o.review_id AND l.observation_id=o.observation_id))
             ORDER BY finding_id) f),
          (SELECT json_group_array(json_array(l.original_note_id,t.reviewer_id)) FROM followup_threads t
             JOIN followup_observation_origins l USING(review_id,generation,original_note_id)
             WHERE l.review_id=o.review_id AND l.observation_id=o.observation_id)
         FROM followup_members m JOIN followup_observations o USING(review_id,observation_id)
         WHERE m.review_id=?1 AND m.batch_id=?2
         ORDER BY o.observed_at_ms,o.observation_id LIMIT ?3",
    )?;
    let mut rows = statement.query(params![review.as_str(), batch, MAX_FROZEN_OBSERVATIONS])?;
    let mut output = Vec::new();
    while let Some(row) = rows.next()? {
        output.push(CheckMember {
            observation_id: row.get(0)?,
            discussion_id: row.get(1)?,
            discussion_version: row.get(2)?,
            originals: {
                let raw: Vec<(String, String)> =
                    serde_json::from_str(text(row, 6)?).map_err(|_| AppError::Internal)?;
                let mut values = raw
                    .into_iter()
                    .map(|(note, reviewer)| {
                        Ok(SelectedNote {
                            note_id: note.parse().map_err(|_| AppError::Internal)?,
                            reviewer_id: reviewer.parse().map_err(|_| AppError::Internal)?,
                        })
                    })
                    .collect::<Result<Vec<_>, StoreError>>()?;
                values.sort_unstable();
                if values.is_empty() {
                    return Err(AppError::Conflict.into());
                }
                values
            },
            prior_batch_id: row.get(4)?,
            finding_ids: serde_json::from_str(text(row, 5)?).map_err(|_| AppError::Internal)?,
        });
    }
    Ok(output)
}

fn free_slots(db: &Connection) -> Result<u8, StoreError> {
    let count: i64 = db.query_row(
        "SELECT count(*) FROM followup_batches WHERE authority_verified=1 AND holds_slot=1",
        [],
        |row| row.get(0),
    )?;
    let cap = preferences(db)?.concurrency;
    Ok(cap.saturating_sub(u8::try_from(count).unwrap_or(u8::MAX)))
}

fn reserve(tx: &mut WriteTx<'_>, check: PreparedCheck) -> Result<(), StoreError> {
    let db = &tx.sql;
    let pending = &check.pending;
    let target = &pending.monitor;
    let when = now_ms()?;
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM followup_batches b JOIN followup_monitors m USING(review_id)
         JOIN reviews r USING(review_id) WHERE b.batch_id=?1 AND b.review_id=?2 AND b.run_id=?3
           AND r.head_sha=?4 AND b.generation=?5 AND b.due_at_ms<=?6 AND {ELIGIBLE})"
    );
    let current: bool = db.query_row(
        &sql,
        params![
            pending.batch_id,
            target.review.as_str(),
            target.run.as_str(),
            check.checked_head.as_str(),
            target.generation.as_sqlite_integer(),
            when
        ],
        |row| row.get(0),
    )?;
    if !current
        || settings_revision(db)? != target.settings
        || lead_for_run(db, &target.run)?.as_ref() != Some(&pending.lead)
    {
        return Err(AppError::StaleRevision.into());
    }
    if free_slots(db)? == 0 {
        return Err(AppError::Busy.into());
    }
    let (base, version) = baseline(db, &target.run)?;
    let selected = members(db, &target.review, &pending.batch_id)?;
    if base != pending.base || version != pending.disposition_version || selected != pending.members
    {
        return Err(AppError::StaleRevision.into());
    }
    let submission: SubmissionId = parse(&Uuid::new_v4().to_string())?;
    let manifest = FrozenManifest {
        batch_id: pending.batch_id.clone(),
        submission_id: submission.to_string(),
        generation: target.generation.get(),
        source_sha: check.checked_head.to_string(),
        target_sha: None,
        report_receipt_id: base.as_ref().map(|base| base.receipt_id.to_string()),
        report_hash: base.as_ref().map(|base| base.hash.to_string()),
        disposition_version: version.get(),
        model: check.ready.model.clone(),
        effort: check.ready.effort.clone(),
        observations: selected
            .into_iter()
            .map(|member| FrozenMember {
                id: member.observation_id,
                discussion_id: member.discussion_id,
                discussion_version: member.discussion_version,
                originals: member
                    .originals
                    .into_iter()
                    .map(|original| FrozenOriginal {
                        note_id: original.note_id,
                        reviewer_id: original.reviewer_id,
                    })
                    .collect(),
                finding_ids: member.finding_ids,
                prior_batch_id: member.prior_batch_id,
            })
            .collect(),
    };
    let manifest_json = serde_json::to_string(&manifest).map_err(|_| AppError::Internal)?;
    if manifest_json.len() > 128 * 1024 {
        return Err(AppError::PayloadTooLarge.into());
    }
    db.execute(
        "UPDATE followup_batches SET state='reserved',holds_slot=1,source_sha=?2,
             base_receipt_id=?3,base_sha256=?4,disposition_version=?5,private_manifest=?6,
             submission_id=?7,hold_reason=NULL,lease_fence=lease_fence+1,lease_until_ms=NULL
         WHERE batch_id=?1",
        params![
            pending.batch_id,
            check.checked_head.as_str(),
            manifest.report_receipt_id,
            manifest.report_hash,
            version.as_sqlite_integer(),
            manifest_json,
            manifest.submission_id
        ],
    )?;
    split_successor(db, pending, &manifest, when)?;
    admit_workflow(
        tx,
        WorkflowAdmission {
            target: pending.lead.clone(),
            submission,
            body: check.prompt,
            binding: WorkflowBinding::Followup(pending.batch_id.clone()),
            settings: target.settings,
            head: check.checked_head,
            ready: check.ready,
        },
    )?;
    emit(tx, &target.review, &target.run)
}

fn split_successor(
    db: &Connection,
    pending: &PendingCheck,
    manifest: &FrozenManifest,
    time: i64,
) -> Result<(), StoreError> {
    let keep = serde_json::to_string(
        &manifest
            .observations
            .iter()
            .map(|member| member.id.as_str())
            .collect::<Vec<_>>(),
    )
    .map_err(|_| AppError::Internal)?;
    let interval: Option<(i64, i64)> = db
        .query_row(
            "SELECT min(o.observed_at_ms),max(o.observed_at_ms) FROM followup_members m
         JOIN followup_observations o USING(review_id,observation_id)
         WHERE m.review_id=?1 AND m.batch_id=?2
           AND m.observation_id NOT IN (SELECT value FROM json_each(?3))
         HAVING count(*)>0",
            params![pending.monitor.review.as_str(), pending.batch_id, keep],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((first, last)) = interval else {
        return Ok(());
    };
    let new_id = Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO followup_batches(batch_id,review_id,run_id,lead_session_key,generation,state,
           authority_verified,holds_slot,manual_requested,due_at_ms,first_observed_at_ms,latest_observed_at_ms)
         SELECT ?2,review_id,run_id,lead_session_key,generation,'pending',1,0,manual_requested,?3,?4,?5
         FROM followup_batches WHERE batch_id=?1",
        params![pending.batch_id,new_id,time,first,last],
    )?;
    db.execute(
        "UPDATE followup_members SET batch_id=?4 WHERE review_id=?1 AND batch_id=?2
           AND observation_id NOT IN (SELECT value FROM json_each(?3))",
        params![
            pending.monitor.review.as_str(),
            pending.batch_id,
            keep,
            new_id
        ],
    )?;
    Ok(())
}

fn emit(tx: &mut WriteTx<'_>, review: &ReviewId, run: &RunId) -> Result<(), StoreError> {
    advance_queue(&tx.sql)?;
    tx.emit(&[
        ResourceKey::Followup(Some(review.clone())),
        ResourceKey::Run(Some(run.clone())),
        ResourceKey::Review(Some(review.clone())),
        ResourceKey::Queue,
    ])?;
    Ok(())
}
