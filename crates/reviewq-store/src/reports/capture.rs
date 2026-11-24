use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::reports::{
    AcceptedReport, ArtifactChunk, CapturedArtifact, Declaration, HandoffFieldError,
    HandoffReceipt, ReportAuthority, ReportCaptureStore, ReportReference, ReportWork,
    RevisionEvidence, Source,
};
use reviewq_app::workflow::{
    ExpectedAcknowledgement, NativeCall, ReportFailureAddress, WorkflowTool,
};
use reviewq_domain::identity::{MrIdentity, NativeInputId, ReportHash, RunId, SessionKey};
use reviewq_domain::session::{AbsolutePath, LeadTarget};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::{address, optional};
use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::{lead_for_run, parse, text};
use crate::workspace::{configured, settings_revision};
use crate::{SqliteStore, StoreError};

mod accept;
mod authority;
mod batch;
mod checks;
mod edits;
mod failures;
mod reconciliation;
mod structured;

const CLAIM_MS: i64 = 60_000;
const RECHECK_MS: i64 = 2_000;

impl SqliteStore {
    /// Reconciles pending native handoffs promptly after reopening the exclusive profile.
    pub async fn recover_report_claims(&self) -> Result<(), StoreError> {
        self.write(|tx| {
            tx.sql.execute("UPDATE report_handoffs SET due_at_ms=min(due_at_ms, ?1) WHERE state='pending_completion'", [now_ms()?])?;
            Ok(())
        }).await
    }

    pub async fn next_report_due(&self) -> Result<Option<i64>, StoreError> {
        self.read(|db| {
            Ok(db.query_row(
                "SELECT due_at_ms FROM report_handoffs WHERE state='pending_completion' ORDER BY due_at_ms,receipt_id LIMIT 1",
                [],
                |row| row.get(0),
            ).optional()?)
        }).await
    }
}

impl ReportCaptureStore for SqliteStore {
    fn report_lead(&self, session: SessionKey) -> AppFuture<'_, LeadTarget> {
        Box::pin(async move {
            self.read(move |db| lead_for_session(db, &session))
                .await
                .map_err(StoreError::app)
        })
    }

    fn mcp_report_lead(
        &self,
        actor: Actor,
        provider: reviewq_domain::session::HostProvider,
        native_id: reviewq_domain::identity::NativeSessionId,
    ) -> AppFuture<'_, LeadTarget> {
        Box::pin(async move {
            self.read(move |db| authority::checkout_lead(db, &actor, provider, native_id.as_str()))
                .await
                .map_err(StoreError::app)
        })
    }

    fn report_hook_order(
        &self,
        session: SessionKey,
        turn: reviewq_domain::identity::NativeTurnId,
        call_id: String,
    ) -> AppFuture<'_, (u32, Option<u32>)> {
        Box::pin(async move {
            self.read(move |db| {
                let lead = lead_for_session(db, &session)?;
                if lead.binding().identity().provider()
                    != reviewq_domain::session::HostProvider::Codex
                {
                    return Err(AppError::Forbidden.into());
                }
                let mut query = db.prepare_cached(
                    "SELECT source_generation, call_id FROM report_handoffs
                     WHERE run_id=?1 AND lead_session_key=?2 AND turn_id=?3",
                )?;
                let mut rows = query.query(params![
                    lead.run_id().as_str(),
                    session.as_str(),
                    turn.as_str()
                ])?;
                let (mut largest, mut previous) = (0, None);
                while let Some(row) = rows.next()? {
                    let (_, ordinal) = address::decode(text(row, 0)?);
                    largest = largest.max(ordinal);
                    if text(row, 1)? == call_id && ordinal != 0 {
                        previous = Some(ordinal);
                    }
                }
                Ok((largest, previous))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn report_source(&self, lead: LeadTarget) -> AppFuture<'_, (MrIdentity, ReportAuthority)> {
        Box::pin(async move {
            self.read(move |db| {
                if lead_for_session(db, lead.session_key())? != lead {
                    return Err(AppError::NotFound.into());
                }
                let identity = crate::queue::identity_for_review(db, lead.review_id())?;
                let authority =
                    authority::for_project(db, &identity)?.ok_or(AppError::Unavailable)?;
                Ok((identity, authority))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn accepted_report(&self, lead: LeadTarget) -> AppFuture<'_, Snapshot<Option<AcceptedReport>>> {
        Box::pin(async move {
            self.read(move |db| {
                if lead_for_session(db, lead.session_key())? != lead {
                    return Err(AppError::NotFound.into());
                }
                let value = accepted(db, &lead)?;
                snapshot(db, value)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn stage_report(
        &self,
        call: NativeCall,
        declaration: Declaration,
        request_sha256: ReportHash,
        declaration_sha256: ReportHash,
        source_authority: Option<ReportAuthority>,
        checkout: Option<Actor>,
        headings: Option<Vec<String>>,
    ) -> AppFuture<'_, Snapshot<HandoffReceipt>> {
        Box::pin(async move {
            self.write(move |tx| {
                if let Some(actor) = &checkout {
                    let host = call.lead.binding().identity();
                    if authority::checkout_lead(
                        &tx.sql,
                        actor,
                        host.provider(),
                        host.native_id().as_str(),
                    )? != call.lead
                    {
                        return Err(AppError::Forbidden.into());
                    }
                }
                stage(
                    tx,
                    call,
                    declaration,
                    request_sha256,
                    declaration_sha256,
                    source_authority,
                    checkout.as_ref(),
                    headings.as_deref(),
                )
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn reserve_report_correction(&self, call: NativeCall) -> AppFuture<'_, bool> {
        Box::pin(async move {
            self.write(move |tx| {
                let db=&tx.sql;
                check_call(db,&call)?;
                if !configured(db,&crate::queue::identity_for_review(db,call.lead.review_id())?)? {
                    return Err(AppError::Unavailable.into());
                }
                let time = now_ms()?;
                let inserted=db.execute(
                    "INSERT INTO report_correction_requests(run_id,lead_session_key,turn_id,call_id,source_generation,source_position,requested_at_ms)
                     VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT DO NOTHING",
                    params![call.lead.run_id().as_str(),call.lead.session_key().as_str(),call.turn_id.as_str(),
                        call.call_id,address::encode(&call).as_ref(),db_int(call.source_position)?,time],
                )?;
                if inserted == 1
                    && let Some(notification) = failures::note(db, &call, Uuid::new_v4().to_string(),
                        "report_correction_required", "report_delivery_rejection", time)?
                {
                    crate::workspace::advance_queue(db)?;
                    tx.emit(&[ResourceKey::Queue, ResourceKey::Report(Some(call.lead.run_id().clone())),
                        ResourceKey::Review(Some(call.lead.review_id().clone())),
                        ResourceKey::Run(Some(call.lead.run_id().clone())), ResourceKey::Notification(Some(notification))])?;
                }
                Ok(inserted==1)
            }).await.map_err(StoreError::app)
        })
    }

    fn report_handoff_status(
        &self,
        call: NativeCall,
        digest: ReportHash,
    ) -> AppFuture<'_, Snapshot<HandoffReceipt>> {
        Box::pin(async move {
            self.read(move |db| {
                check_call(db, &call)?;
                let receipt = saved(db, &call, &digest)?.ok_or(AppError::NotFound)?;
                snapshot(db, receipt)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn claim_report_work(&self, limit: u16) -> AppFuture<'_, Vec<ReportWork>> {
        Box::pin(async move {
            if !(1..=4).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.write(move |tx| claim(&tx.sql, limit))
                .await
                .map_err(StoreError::app)
        })
    }

    fn defer_report(&self, work: ReportWork) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                ensure_claim(&tx.sql, &work)?;
                tx.sql.execute(
                    "UPDATE report_handoffs SET due_at_ms=?2 WHERE receipt_id=?1",
                    params![
                        work.acknowledgement.receipt_id.as_str(),
                        now_ms()?.saturating_add(RECHECK_MS)
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn begin_artifact(&self, work: ReportWork) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                let old = ensure_claim(&tx.sql, &work)?;
                let artifact = Uuid::new_v4().to_string();
                let time = now_ms()?;
                tx.sql.execute(
                    "INSERT INTO artifacts(artifact_id, byte_length, total_code_points, line_count,
                         max_line_bytes, complete, sealed, created_at_ms)
                     VALUES(?1, 0, 0, 0, 0, 0, 0, ?2)",
                    params![artifact, time],
                )?;
                tx.sql.execute(
                    "UPDATE report_handoffs SET artifact_id=?2, updated_at_ms=max(updated_at_ms, ?3)
                     WHERE receipt_id=?1",
                    params![work.acknowledgement.receipt_id.as_str(), artifact, time],
                )?;
                if let Some(old) = old {
                    tx.sql.execute(
                        "DELETE FROM artifacts WHERE artifact_id=?1 AND sealed=0",
                        [old],
                    )?;
                } else {
                    tx.emit(&[ResourceKey::Report(Some(work.call.lead.run_id().clone()))])?;
                }
                Ok(())
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn append_report_chunk(&self, work: ReportWork, chunk: ArtifactChunk) -> AppFuture<'_, ()> {
        Box::pin(async move {
            if chunk.bytes.is_empty() || chunk.bytes.len() > 64 * 1024 {
                return Err(AppError::InvalidRequest);
            }
            let scalars = std::str::from_utf8(&chunk.bytes)
                .map_err(|_| AppError::InvalidRequest)?
                .chars()
                .count() as i64;
            self.write(move |tx| {
                let artifact = ensure_claim(&tx.sql, &work)?.ok_or(AppError::Conflict)?;
                let ord = db_int(chunk.ordinal)?;
                let start = db_int(chunk.byte_start)?;
                let scalar_start = db_int(chunk.scalar_start)?;
                let valid = tx.sql.execute(
                    "UPDATE artifacts SET byte_length=byte_length+?4, total_code_points=total_code_points+?5
                     WHERE artifact_id=?1 AND sealed=0 AND byte_length=?2 AND total_code_points=?3
                       AND ?6=(SELECT coalesce(max(ordinal), -1)+1 FROM artifact_chunks WHERE artifact_id=?1)",
                    params![artifact, start, scalar_start, chunk.bytes.len() as i64, scalars, ord],
                )?;
                if valid != 1 {
                    return Err(AppError::Conflict.into());
                }
                tx.sql.execute(
                    "INSERT INTO artifact_chunks(artifact_id, ordinal, byte_start, scalar_start, raw)
                     VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![artifact, ord, start, scalar_start, chunk.bytes],
                )?;
                Ok(())
            }).await.map_err(StoreError::app)
        })
    }

    fn accept_report(
        &self,
        work: ReportWork,
        result_entry: NativeInputId,
        artifact: Option<CapturedArtifact>,
        resolved: Vec<ReportFailureAddress>,
    ) -> AppFuture<'_, bool> {
        Box::pin(async move {
            self.write(move |tx| accept::complete(tx, work, result_entry, artifact, resolved))
                .await
                .map_err(StoreError::app)
        })
    }

    fn index_captured_source(
        &self,
        run: RunId,
        reference: ReportReference,
        source: Option<String>,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.cache_report_display(run, reference, source)
                .await
                .map_err(StoreError::app)
        })
    }

    fn unresolved_report_failures(
        &self,
        call: NativeCall,
    ) -> AppFuture<'_, Vec<ReportFailureAddress>> {
        Box::pin(async move {
            self.read(move |db| {
                check_call(db, &call)?;
                failures::unresolved(db, &call)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn reject_report(
        &self,
        work: ReportWork,
        artifact: Option<CapturedArtifact>,
        reason: &'static str,
        native_result: Option<NativeInputId>,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                accept::reject(tx, &work, artifact.as_ref(), reason, native_result.as_ref())
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn accepted(db: &Connection, lead: &LeadTarget) -> Result<Option<AcceptedReport>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT v.receipt_id, a.sha256, c.source_kind, c.source_path
         FROM report_heads h JOIN report_versions v USING(run_id, receipt_id)
         JOIN artifacts a USING(artifact_id)
         LEFT JOIN report_handoffs c ON c.run_id=v.run_id AND c.receipt_id=v.handoff_receipt_id
         WHERE h.run_id=?1 AND a.sealed=1",
    )?;
    let mut rows = statement.query([lead.run_id().as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let reference = ReportReference {
        receipt_id: parse(text(row, 0)?)?,
        hash: parse(text(row, 1)?)?,
    };
    let source = match row.get::<_, Option<String>>(2)?.as_deref() {
        Some("response") => Some(Source::Response),
        Some("file") => Some(Source::File {
            path: AbsolutePath::from_canonical(PathBuf::from(OsString::from_vec(row.get(3)?)))
                .map_err(|_| AppError::Internal)?,
            hash: Some(reference.hash.clone()),
        }),
        None => None,
        _ => return Err(AppError::Internal.into()),
    };
    Ok(Some(AcceptedReport { reference, source }))
}

fn lead_for_session(db: &Connection, session: &SessionKey) -> Result<LeadTarget, StoreError> {
    let run = db
        .query_row(
            "SELECT run_id FROM runs WHERE lead_session_key=?1",
            [session.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(AppError::NotFound)?;
    let lead = lead_for_run(db, &parse(&run)?)?.ok_or(AppError::NotFound)?;
    if lead.session_key() != session {
        return Err(AppError::NotFound.into());
    }
    Ok(lead)
}

fn check_call(db: &Connection, call: &NativeCall) -> Result<(), StoreError> {
    if call.tool != WorkflowTool::Report
        || call.call_id.is_empty()
        || call.call_id.len() > 512
        || call.call_id.chars().any(char::is_control)
        || call.source_generation.is_empty()
        || call.source_generation.len() > 512
    {
        return Err(AppError::InvalidRequest.into());
    }
    if lead_for_session(db, call.lead.session_key())? != call.lead {
        return Err(AppError::NotFound.into());
    }
    Ok(())
}

fn saved(
    db: &Connection,
    call: &NativeCall,
    digest: &ReportHash,
) -> Result<Option<HandoffReceipt>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT receipt_id, state, request_sha256, source_generation, source_position,
                request_entry_id, declaration_sha256, root_turn_id, json_extract(edits, '$.argumentsSha256') FROM report_handoffs
         WHERE run_id=?1 AND lead_session_key=?2 AND turn_id=?3 AND call_id=?4",
    )?;
    let mut rows = statement.query(params![
        call.lead.run_id().as_str(),
        call.lead.session_key().as_str(),
        call.turn_id.as_str(),
        call.call_id,
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    if text(row, 2)? != digest.as_str()
        || text(row, 3)? != address::encode(call).as_ref()
        || row.get::<_, i64>(4)? != db_int(call.source_position)?
        || optional::<NativeInputId>(row, 5)? != call.request_entry_id
        || optional::<ReportHash>(row, 8)? != call.arguments_sha256
        || optional::<reviewq_domain::identity::NativeTurnId>(row, 7)? != call.root_turn_id
    {
        return Err(AppError::IdempotencyConflict.into());
    }
    Ok(Some(HandoffReceipt {
        receipt_id: parse(text(row, 0)?)?,
        declaration_sha256: parse(text(row, 6)?)?,
        state: parse(text(row, 1)?)?,
    }))
}

#[allow(clippy::too_many_arguments)]
fn stage(
    tx: &mut WriteTx<'_>,
    call: NativeCall,
    declaration: Declaration,
    request_sha256: ReportHash,
    declaration_sha256: ReportHash,
    source_authority: Option<ReportAuthority>,
    checkout: Option<&Actor>,
    headings: Option<&[String]>,
) -> Result<Snapshot<HandoffReceipt>, StoreError> {
    let db = &tx.sql;
    check_call(db, &call)?;
    if let Some(receipt) = saved(db, &call, &request_sha256)? {
        return snapshot(db, receipt);
    }
    let identity = crate::queue::identity_for_review(db, call.lead.review_id())?;
    let authority = authority::for_project(db, &identity)?.ok_or(AppError::Unavailable)?;
    let settings = settings_revision(db)?;
    if declaration.requires_review_refresh() && source_authority.as_ref() != Some(&authority) {
        return Err(AppError::StaleRevision.into());
    }
    let run = call.lead.run_id();
    let (base, version) = db
        .query_row(
            "SELECT receipt_id, disposition_version FROM report_heads WHERE run_id=?1",
            [run.as_str()],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .unwrap_or((None, 0));
    if let Some(hold) = crate::hosts::unresolved_workflow_input(db, &call.lead, &call.turn_id)? {
        return Err(hold.into());
    }
    let scoped_batch = batch::bound(db, &call)?;
    // Assessments without a bound batch and a bound batch's missing assessments are field
    // errors below; an amendment cannot complete the batch at all.
    if scoped_batch.is_some()
        && matches!(
            declaration,
            Declaration::Final {
                amends: Some(_),
                ..
            }
        )
    {
        return Err(AppError::Conflict.into());
    }
    if batch::action_turn(db, &call)?
        && matches!(declaration, Declaration::Final { amends: None, .. })
    {
        return Err(AppError::Forbidden.into());
    }
    let assessments = batch::encode(declaration.assessments())?;
    let (mut errors, projection) = checks::ledger(
        db,
        run.as_str(),
        base.as_deref(),
        scoped_batch.as_deref(),
        &declaration,
        headings,
    )?;
    if scoped_batch.is_none() && !declaration.assessments().is_empty() {
        errors.push(checks::unbound_assessments());
    }
    let (form, source, revision, verdict, expected, reason, edits) = match declaration {
        Declaration::Final {
            source,
            revision,
            verdict,
            amends,
            edits,
            report,
            ..
        } => {
            let metadata = edits::encode(&edits, report.as_ref())?;
            (
                "final",
                Some(source),
                Some(revision),
                Some(verdict),
                amends,
                None,
                metadata,
            )
        }
        Declaration::Unchanged {
            accepted, source, ..
        } => (
            "unchanged",
            Some(source),
            None,
            None,
            Some(accepted),
            None,
            None,
        ),
        Declaration::NeedsInput { reason, .. } => {
            ("needs_input", None, None, None, None, Some(reason), None)
        }
    };
    let (kind, path, hash) = match source {
        Some(Source::File { path, hash }) => (Some("file"), Some(path), hash),
        Some(Source::Response) => (Some("response"), None, None),
        None => (None, None, None),
    };
    let (head, revision_reason) = match revision {
        Some(RevisionEvidence::Exact(sha)) => (Some(sha), None),
        Some(RevisionEvidence::Unknown(reason)) => (None, Some(reason)),
        None => (None, None),
    };
    let source_generation = address::encode(&call);
    let source_position = db_int(call.source_position)?;
    // Terminal receipts cannot reopen. Pin the incomparable ones already
    // recorded when this verified active call enters the store.
    let incomparable_recorded: i64 = db.query_row(
        "SELECT count(*) FROM report_handoffs WHERE run_id=?1 AND state='recorded'
           AND reviewq_report_order(source_generation,source_position,?2,?3) IS NULL",
        params![run.as_str(), source_generation.as_ref(), source_position],
        |row| row.get(0),
    )?;
    let edits = authority::bind(
        db,
        edits.as_deref(),
        &authority,
        checkout,
        call.arguments_sha256.as_ref(),
        incomparable_recorded,
    )?;
    if let Some(projection) = &projection
        && !structured::fits(db, &edits, projection.metadata())?
    {
        errors.push(checks::too_large());
    }
    if !errors.is_empty() {
        return Err(AppError::InvalidHandoff(HandoffFieldError::bounded(errors)).into());
    }
    let receipt_id = Uuid::new_v4().to_string();
    let time = now_ms()?;
    db.execute(
        "INSERT INTO report_handoffs(receipt_id, run_id, lead_session_key, turn_id, call_id,
           source_generation, request_entry_id, declaration_sha256, form, source_kind, source_path,
           source_hash, reviewed_sha, revision_reason, verdict, expected_receipt_id, expected_sha256,
           base_receipt_id, disposition_version, reason, state, due_at_ms, created_at_ms, updated_at_ms,
           source_position, request_sha256, settings_revision, batch_id, assessments, edits, root_turn_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,
                'pending_completion',?21,?21,?21,?22,?23,?24,?25,?26,?27,?28)",
        params![
            receipt_id, run.as_str(), call.lead.session_key().as_str(), call.turn_id.as_str(), call.call_id,
            source_generation.as_ref(), call.request_entry_id.as_ref().map(|id| id.as_str()), declaration_sha256.as_str(),
            form, kind, path.as_ref().map(|path| path.as_path().as_os_str().as_bytes()),
            hash.as_ref().map(|hash| hash.as_str()), head.as_ref().map(|sha| sha.as_str()), revision_reason,
            verdict.map(|value| value.to_string()), expected.as_ref().map(|value| value.receipt_id.as_str()),
            expected.as_ref().map(|value| value.hash.as_str()), base, version, reason, time,
            source_position, request_sha256.as_str(), settings.as_sqlite_integer(), scoped_batch, assessments, edits,
            call.root_turn_id.as_ref().map(|turn| turn.as_str()),
        ],
    )?;
    if let Some(batch) = scoped_batch
        && batch::continue_on_turn(&tx.sql, &batch, &call)?
    {
        tx.emit(&[ResourceKey::Followup(Some(call.lead.review_id().clone()))])?;
    }
    crate::workspace::advance_queue(&tx.sql)?;
    tx.emit(&[
        ResourceKey::Report(Some(run.clone())),
        ResourceKey::Run(Some(run.clone())),
        ResourceKey::Queue,
    ])?;
    tx.report_changed = true;
    snapshot(
        &tx.sql,
        HandoffReceipt {
            receipt_id: parse(&receipt_id)?,
            declaration_sha256,
            state: reviewq_app::reports::HandoffState::PendingCompletion,
        },
    )
}

fn claim(db: &Connection, limit: u16) -> Result<Vec<ReportWork>, StoreError> {
    let now = now_ms()?;
    let mut query = db.prepare_cached(
        "SELECT receipt_id, run_id, lead_session_key, turn_id, call_id, source_generation,
            request_entry_id, declaration_sha256, form, source_kind, source_path, source_hash,
            reviewed_sha, revision_reason, verdict, expected_receipt_id, expected_sha256, reason, fence, source_position, assessments, edits, root_turn_id, json_extract(edits, '$.argumentsSha256')
         FROM report_handoffs WHERE state='pending_completion' AND due_at_ms<=?1
         ORDER BY due_at_ms, receipt_id LIMIT ?2",
    )?;
    let mut rows = query.query(params![now, limit])?;
    let mut output = Vec::new();
    while let Some(row) = rows.next()? {
        let lead = lead_for_session(db, &parse(text(row, 2)?)?)?;
        if lead.run_id().as_str() != text(row, 1)? {
            return Err(AppError::Internal.into());
        }
        let fence = row
            .get::<_, i64>(18)?
            .checked_add(1)
            .ok_or(AppError::Internal)?;
        let (generation, source_ordinal) = address::decode(text(row, 5)?);
        output.push(ReportWork {
            call: NativeCall {
                lead,
                turn_id: parse(text(row, 3)?)?,
                root_turn_id: optional(row, 22)?,
                call_id: row.get(4)?,
                source_generation: generation.to_owned(),
                source_ordinal,
                arguments_sha256: optional(row, 23)?,
                source_position: u64::try_from(row.get::<_, i64>(19)?)
                    .map_err(|_| AppError::Internal)?,
                request_entry_id: optional(row, 6)?,
                tool: WorkflowTool::Report,
            },
            acknowledgement: ExpectedAcknowledgement {
                receipt_id: parse(text(row, 0)?)?,
                declaration_sha256: parse(text(row, 7)?)?,
            },
            declaration: declaration(row)?,
            fence: fence as u64,
        });
    }
    for work in &output {
        db.execute(
            "UPDATE report_handoffs SET fence=?2, due_at_ms=?3 WHERE receipt_id=?1",
            params![
                work.acknowledgement.receipt_id.as_str(),
                db_int(work.fence)?,
                now.saturating_add(CLAIM_MS)
            ],
        )?;
    }
    Ok(output)
}

fn declaration(row: &Row<'_>) -> Result<Declaration, StoreError> {
    let source = match row.get::<_, Option<String>>(9)?.as_deref() {
        Some("file") => Some(Source::File {
            path: AbsolutePath::from_canonical(PathBuf::from(OsString::from_vec(row.get(10)?)))
                .map_err(|_| AppError::Internal)?,
            hash: optional(row, 11)?,
        }),
        Some("response") => Some(Source::Response),
        None => None,
        _ => return Err(AppError::Internal.into()),
    };
    let assessments = batch::decode(text(row, 20)?)?;
    let expected = optional(row, 15)?
        .map(|id| {
            Ok::<_, StoreError>(ReportReference {
                receipt_id: id,
                hash: parse(text(row, 16)?)?,
            })
        })
        .transpose()?;
    Ok(match text(row, 8)? {
        "final" => {
            let metadata: Option<String> = row.get(21)?;
            let (edits, report) = edits::decode(metadata.as_deref())?;
            Declaration::Final {
                source: source.ok_or(AppError::Internal)?,
                revision: match optional(row, 12)? {
                    Some(sha) => RevisionEvidence::Exact(sha),
                    None => RevisionEvidence::Unknown(row.get(13)?),
                },
                verdict: parse(text(row, 14)?)?,
                report,
                amends: expected,
                assessments,
                edits,
            }
        }
        "unchanged" => Declaration::Unchanged {
            accepted: expected.ok_or(AppError::Internal)?,
            assessments,
            source: source.ok_or(AppError::Internal)?,
        },
        "needs_input" => Declaration::NeedsInput {
            reason: row.get(17)?,
            assessments,
        },
        _ => return Err(AppError::Internal.into()),
    })
}

fn ensure_claim(db: &Connection, work: &ReportWork) -> Result<Option<String>, StoreError> {
    db.query_row(
        "SELECT artifact_id FROM report_handoffs
         WHERE receipt_id=?1 AND run_id=?2 AND lead_session_key=?3 AND turn_id=?4 AND call_id=?5
           AND source_generation=?6 AND declaration_sha256=?7 AND fence=?8 AND state='pending_completion'",
        params![
            work.acknowledgement.receipt_id.as_str(), work.call.lead.run_id().as_str(),
            work.call.lead.session_key().as_str(), work.call.turn_id.as_str(), work.call.call_id,
            address::encode(&work.call).as_ref(), work.acknowledgement.declaration_sha256.as_str(), db_int(work.fence)?,
        ], |row| row.get(0),
    ).optional()?.ok_or_else(|| AppError::Conflict.into())
}

fn db_int(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| AppError::Internal.into())
}
