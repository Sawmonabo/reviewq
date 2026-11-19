//! Scoped reads over accepted report versions and their immutable bytes.

mod context;

use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::reports::{
    ArtifactStats, CandidateReport, ContextRequest, MAX_REPORT_PAGE, MAX_SECTION_SCALARS,
    ReportContext, ReportDelivery, ReportHistory, ReportPosition, ReportReference, ReportSection,
    ReportStore, ReportSummary, ReportView,
};
use reviewq_domain::identity::{ReceiptId, RunId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::events::snapshot;
use crate::queue::{identity_for_review, lead_for_run, parse, text};
use crate::workspace::{allow_review, scope};
use crate::{SqliteStore, StoreError};

struct Version {
    summary: ReportSummary,
    artifact_id: String,
}

const VERSION_SELECT: &str = "
    SELECT v.receipt_id, a.sha256, v.parent_receipt_id, v.attention_receipt_id,
        v.reviewed_sha, v.revision_reason, v.full_mr_reviewed_sha, v.coverage, v.verdict,
        v.semantic_index, v.finding_count, v.change_count, v.captured_at_ms, v.artifact_id,
        a.byte_length, a.total_code_points, a.line_count, a.max_line_bytes, a.complete,
        a.original_sha256, v.handoff_receipt_id IS NOT NULL
    FROM report_versions v JOIN artifacts a USING(artifact_id)
    WHERE v.run_id=?1 AND a.sealed=1";

impl ReportStore for SqliteStore {
    fn read_report(
        &self,
        actor: Actor,
        run: RunId,
        receipt: Option<ReceiptId>,
        markdown: bool,
    ) -> AppFuture<'_, Snapshot<ReportView>> {
        Box::pin(async move {
            self.read(move |tx| {
                authorize_run(tx, &actor, &run)?;
                let selected = resolve_receipt(tx, &run, receipt)?;
                let Some(wanted) = selected else {
                    return snapshot(
                        tx,
                        ReportView {
                            report: None,
                            delivery: read_delivery(tx, &run)?,
                            markdown: None,
                        },
                    );
                };
                let version = version(tx, &run, &wanted)?;
                let body = markdown.then(|| read_markdown(tx, &version)).transpose()?;
                snapshot(
                    tx,
                    ReportView {
                        report: Some(version.summary),
                        delivery: read_delivery(tx, &run)?,
                        markdown: body,
                    },
                )
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_report_history(
        &self,
        actor: Actor,
        run: RunId,
        after: Option<ReportPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ReportHistory>> {
        Box::pin(async move {
            if !(1..=MAX_REPORT_PAGE).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| {
                authorize_run(tx, &actor, &run)?;
                let (time, receipt) = after.as_ref().map_or((None, ""), |p| {
                    (Some(p.captured_at_ms), p.receipt_id.as_str())
                });
                let sql = format!(
                    "{VERSION_SELECT}
                    AND (?2 IS NULL OR (v.captured_at_ms, v.receipt_id) < (?2, ?3))
                    ORDER BY v.captured_at_ms DESC, v.receipt_id DESC LIMIT ?4"
                );
                let mut statement = tx.prepare_cached(&sql)?;
                let mut rows = statement.query(params![run.as_str(), time, receipt, limit + 1])?;
                let mut items = Vec::with_capacity(usize::from(limit));
                let mut next = None;
                while let Some(row) = rows.next()? {
                    if items.len() == usize::from(limit) {
                        next = items.last().map(|item: &ReportSummary| ReportPosition {
                            captured_at_ms: item.captured_at_ms,
                            receipt_id: item.reference.receipt_id.clone(),
                        });
                        break;
                    }
                    items.push(decode_version(row)?.summary);
                }
                snapshot(
                    tx,
                    ReportHistory {
                        items,
                        next,
                        current_receipt_id: current_receipt(tx, &run)?,
                        delivery: read_delivery(tx, &run)?,
                    },
                )
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_report_context(
        &self,
        actor: Actor,
        run: RunId,
        request: ContextRequest,
    ) -> AppFuture<'_, Snapshot<ReportContext>> {
        Box::pin(async move {
            if !(1..=MAX_REPORT_PAGE).contains(&request.limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| {
                authorize_run(tx, &actor, &run)?;
                snapshot(tx, context::read(tx, &run, request)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_report_section(
        &self,
        actor: Actor,
        run: RunId,
        receipt: Option<ReceiptId>,
        offset: u64,
        characters: u32,
    ) -> AppFuture<'_, Snapshot<ReportSection>> {
        Box::pin(async move {
            if !(1..=MAX_SECTION_SCALARS).contains(&characters) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| {
                authorize_run(tx, &actor, &run)?;
                let selected = resolve_receipt(tx, &run, receipt)?.ok_or(AppError::NotFound)?;
                let version = version(tx, &run, &selected)?;
                let value = section(
                    tx,
                    version.summary.reference,
                    version.summary.stats.total_code_points,
                    &version.artifact_id,
                    offset,
                    characters,
                )?;
                snapshot(tx, value)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_report_chunk(
        &self,
        actor: Actor,
        run: RunId,
        report: ReportReference,
        ordinal: u64,
    ) -> AppFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            let ordinal = i64::try_from(ordinal).map_err(|_| AppError::InvalidRequest)?;
            self.read(move |tx| {
                authorize_run(tx, &actor, &run)?;
                let version = version(tx, &run, &report.receipt_id)?;
                if version.summary.reference != report {
                    return Err(AppError::PreconditionFailed.into());
                }
                Ok(tx
                    .query_row(
                        "SELECT raw FROM artifact_chunks WHERE artifact_id=?1 AND ordinal=?2",
                        params![version.artifact_id, ordinal],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_report_candidate(
        &self,
        actor: Actor,
        run: RunId,
        receipt: ReceiptId,
        offset: u64,
        characters: u32,
    ) -> AppFuture<'_, Snapshot<CandidateReport>> {
        Box::pin(async move {
            if !(1..=MAX_SECTION_SCALARS).contains(&characters) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| {
                authorize_run(tx, &actor, &run)?;
                let mut statement = tx.prepare_cached(
                    "SELECT h.artifact_id, a.sha256, h.turn_id, h.call_id, h.declaration_sha256,
                       h.reason, a.byte_length, a.total_code_points, a.line_count, a.max_line_bytes, a.complete
                     FROM report_handoffs h JOIN artifacts a USING(artifact_id)
                     WHERE h.run_id=?1 AND h.receipt_id=?2 AND h.state='failed' AND a.sealed=1"
                )?;
                let mut rows = statement.query(params![run.as_str(), receipt.as_str()])?;
                let row = rows.next()?.ok_or(AppError::NotFound)?;
                let reference = ReportReference { receipt_id: receipt, hash: parse(text(row, 1)?)? };
                let stats = decode_stats(row, 6)?;
                let candidate = CandidateReport {
                    section: section(tx, reference.clone(), stats.total_code_points, text(row, 0)?,
                        offset, characters)?,
                    reference,
                    stats,
                    turn_id: row.get(2)?,
                    call_id: row.get(3)?,
                    declaration_sha256: parse(text(row, 4)?)?,
                    reason: row.get(5)?,
                };
                snapshot(tx, candidate)
            }).await.map_err(StoreError::app)
        })
    }
}

pub(crate) fn authorize_run(
    connection: &Connection,
    actor: &Actor,
    run: &RunId,
) -> Result<(), StoreError> {
    let access = scope(connection, actor)?;
    let lead = lead_for_run(connection, run)?.ok_or(AppError::NotFound)?;
    allow_review(
        connection,
        access,
        lead.review_id(),
        &identity_for_review(connection, lead.review_id())?,
    )?;
    if let ActorScope::NativeSession(identity) = actor.scope()
        && identity != lead.binding().identity()
    {
        return Err(AppError::NotFound.into());
    }
    Ok(())
}

fn current_receipt(connection: &Connection, run: &RunId) -> Result<Option<ReceiptId>, StoreError> {
    let current = connection
        .query_row(
            "SELECT receipt_id FROM report_heads WHERE run_id=?1",
            [run.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    current.as_deref().map(parse).transpose()
}

fn resolve_receipt(
    connection: &Connection,
    run: &RunId,
    receipt: Option<ReceiptId>,
) -> Result<Option<ReceiptId>, StoreError> {
    match receipt {
        Some(receipt) => Ok(Some(receipt)),
        None => current_receipt(connection, run),
    }
}

fn read_delivery(
    connection: &Connection,
    run: &RunId,
) -> Result<Option<ReportDelivery>, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT receipt_id, CASE state
             WHEN 'pending_completion' THEN CASE WHEN artifact_id IS NULL THEN 'preparing' ELSE 'saving' END
             WHEN 'recorded' THEN 'saved' ELSE 'failed' END, reason
         FROM report_handoffs WHERE run_id=?1 ORDER BY created_at_ms DESC, receipt_id DESC LIMIT 1",
    )?;
    let mut rows = statement.query([run.as_str()])?;
    rows.next()?
        .map(|row| {
            Ok(ReportDelivery {
                receipt_id: parse(text(row, 0)?)?,
                state: parse(text(row, 1)?)?,
                reason: row.get(2)?,
            })
        })
        .transpose()
}

fn version(
    connection: &Connection,
    run: &RunId,
    receipt: &ReceiptId,
) -> Result<Version, StoreError> {
    let sql = format!("{VERSION_SELECT} AND v.receipt_id=?2");
    let mut statement = connection.prepare_cached(&sql)?;
    let mut rows = statement.query(params![run.as_str(), receipt.as_str()])?;
    decode_version(rows.next()?.ok_or(AppError::NotFound)?)
}

fn decode_version(row: &Row<'_>) -> Result<Version, StoreError> {
    Ok(Version {
        artifact_id: row.get(13)?,
        summary: ReportSummary {
            reference: ReportReference {
                receipt_id: parse(text(row, 0)?)?,
                hash: parse(text(row, 1)?)?,
            },
            original_sha256: optional(row, 19)?,
            native_provenance: row.get(20)?,
            parent_receipt_id: optional(row, 2)?,
            attention_receipt_id: parse(text(row, 3)?)?,
            reviewed_sha: optional(row, 4)?,
            revision_reason: row.get(5)?,
            full_mr_reviewed_sha: optional(row, 6)?,
            coverage: parse(text(row, 7)?)?,
            verdict: parse(text(row, 8)?)?,
            semantic_index: parse(text(row, 9)?)?,
            finding_count: read_optional_count(row, 10)?,
            change_count: read_optional_count(row, 11)?,
            captured_at_ms: row.get(12)?,
            stats: decode_stats(row, 14)?,
        },
    })
}

fn optional<T: std::str::FromStr>(row: &Row<'_>, column: usize) -> Result<Option<T>, StoreError> {
    row.get_ref(column)?
        .as_str_or_null()
        .map_err(rusqlite::Error::from)?
        .map(parse)
        .transpose()
}

fn read_count(row: &Row<'_>, column: usize) -> Result<u64, StoreError> {
    u64::try_from(row.get::<_, i64>(column)?).map_err(|_| AppError::Internal.into())
}

fn read_optional_count(row: &Row<'_>, column: usize) -> Result<Option<u64>, StoreError> {
    row.get::<_, Option<i64>>(column)?
        .map(u64::try_from)
        .transpose()
        .map_err(|_| AppError::Internal.into())
}

fn decode_stats(row: &Row<'_>, start: usize) -> Result<ArtifactStats, StoreError> {
    Ok(ArtifactStats {
        byte_length: read_count(row, start)?,
        total_code_points: read_count(row, start + 1)?,
        line_count: read_count(row, start + 2)?,
        max_line_bytes: read_count(row, start + 3)?,
        complete: row.get(start + 4)?,
    })
}

fn read_markdown(connection: &Connection, version: &Version) -> Result<String, StoreError> {
    let stats = &version.summary.stats;
    stats.require_markdown_budget()?;
    let mut result = Vec::with_capacity(stats.byte_length as usize);
    let mut statement = connection
        .prepare_cached("SELECT raw FROM artifact_chunks WHERE artifact_id=?1 ORDER BY ordinal")?;
    let mut rows = statement.query([&version.artifact_id])?;
    while let Some(row) = rows.next()? {
        let raw = raw(row, 0)?;
        if result.len() + raw.len() > stats.byte_length as usize {
            return Err(AppError::Internal.into());
        }
        result.extend_from_slice(raw);
    }
    if result.len() as u64 != stats.byte_length {
        return Err(AppError::Internal.into());
    }
    String::from_utf8(result).map_err(|_| AppError::Internal.into())
}

fn section(
    connection: &Connection,
    report: ReportReference,
    total: u64,
    artifact: &str,
    offset: u64,
    characters: u32,
) -> Result<ReportSection, StoreError> {
    if offset > total {
        return Err(AppError::RangeNotSatisfiable.into());
    }
    let end = (offset + u64::from(characters)).min(total);
    let mut text = String::new();
    if offset < end {
        let first = connection.query_row(
            "SELECT ordinal FROM artifact_chunks WHERE artifact_id=?1 AND scalar_start<=?2
             ORDER BY scalar_start DESC LIMIT 1",
            params![artifact, offset as i64],
            |row| row.get::<_, i64>(0),
        )?;
        let mut statement = connection.prepare_cached(
            "SELECT scalar_start, raw FROM artifact_chunks
             WHERE artifact_id=?1 AND ordinal>=?2 AND scalar_start<?3 ORDER BY ordinal",
        )?;
        let mut rows = statement.query(params![artifact, first, end as i64])?;
        let mut next = offset;
        while let Some(row) = rows.next()? {
            let start = read_count(row, 0)?;
            let chunk = std::str::from_utf8(raw(row, 1)?).map_err(|_| AppError::Internal)?;
            let skip = next.checked_sub(start).ok_or(AppError::Internal)? as usize;
            for scalar in chunk.chars().skip(skip).take((end - next) as usize) {
                text.push(scalar);
                next += 1;
            }
        }
        if next != end {
            return Err(AppError::Internal.into());
        }
    }
    Ok(ReportSection {
        report,
        text,
        offset,
        next_offset: (end < total).then_some(end),
        total_code_points: total,
    })
}

fn raw<'row>(row: &'row Row<'_>, column: usize) -> Result<&'row [u8], StoreError> {
    row.get_ref(column)?
        .as_blob()
        .map_err(|_| AppError::Internal.into())
}

/// Check an accepted report attachment and its exact Unicode scalar selection in the admission transaction.
pub(crate) fn authorize_feedback(
    connection: &Connection,
    run: &RunId,
    reference: &reviewq_app::hosts::ReportRef,
) -> Result<Option<ReceiptId>, StoreError> {
    let stored = version(connection, run, &reference.receipt_id)?;
    if stored.summary.reference.hash != reference.report_hash {
        return Err(AppError::PreconditionFailed.into());
    }
    if let Some(quote) = &reference.quote {
        let slice = section(
            connection,
            stored.summary.reference,
            stored.summary.stats.total_code_points,
            &stored.artifact_id,
            quote.offset(),
            quote.length() as u32,
        )?;
        if slice.text != quote.text().as_str() {
            return Err(AppError::PreconditionFailed.into());
        }
    }
    current_receipt(connection, run)
}
