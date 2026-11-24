//! Scoped reads over accepted report versions and their immutable bytes.

pub(crate) mod address;
mod capture;
mod context;
mod mutations;
mod quote;

use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::hosts::MessageBody;
use reviewq_app::reports::{
    ArtifactStats, CandidateReport, ContextRequest, DisplayWindow, DisplayWindowPosition,
    MAX_REPORT_PAGE, MAX_SECTION_SCALARS, QuoteResolution, ReportContext, ReportDelivery,
    ReportHistory, ReportPosition, ReportReference, ReportSection, ReportStore, ReportSummary,
    ReportView,
};
use reviewq_domain::identity::{ReceiptId, ReportHash, RunId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::events::snapshot;
use crate::queue::{identity_for_review, lead_for_run, parse, text};
use crate::workspace::{allow_review, scope};
use crate::{SqliteStore, StoreError};

pub(crate) struct Version {
    pub(crate) summary: ReportSummary,
    pub(crate) artifact_id: String,
}

struct SectionSource<'a> {
    artifact_id: &'a str,
    reference: ReportReference,
    original_sha256: Option<ReportHash>,
    stats: &'a ArtifactStats,
}

const VERSION_SELECT: &str = "
    SELECT v.receipt_id, a.sha256, v.parent_receipt_id, v.attention_receipt_id,
        v.reviewed_sha, v.revision_reason, v.full_mr_reviewed_sha, v.coverage, v.verdict,
        v.semantic_index, v.finding_count, v.change_count, v.captured_at_ms, v.artifact_id,
        a.byte_length, a.total_code_points, a.line_count, a.max_line_bytes, a.complete,
        a.original_sha256, v.handoff_receipt_id IS NOT NULL, v.coverage_source_sha, v.coverage_target_sha,
        (SELECT count(*) FROM report_findings f
         WHERE f.run_id=v.run_id AND f.receipt_id=v.receipt_id AND f.scalar_offset IS NULL)
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

    fn read_report_window(
        &self,
        actor: Actor,
        run: RunId,
        report: ReportReference,
        position: DisplayWindowPosition,
    ) -> AppFuture<'_, Snapshot<DisplayWindow>> {
        Box::pin(async move {
            self.report_display_window(actor, run, report, position)
                .await
                .map_err(StoreError::app)
        })
    }

    fn resolve_report_quote(
        &self,
        actor: Actor,
        run: RunId,
        report: ReportReference,
        text: MessageBody,
    ) -> AppFuture<'_, Snapshot<QuoteResolution>> {
        Box::pin(async move {
            self.read(move |tx| {
                let version = authorized_version(tx, &actor, &run, &report)?;
                snapshot(tx, quote::resolve(tx, &version, text.as_str())?)
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
                    SectionSource {
                        artifact_id: &version.artifact_id,
                        reference: version.summary.reference,
                        original_sha256: version.summary.original_sha256,
                        stats: &version.summary.stats,
                    },
                    offset,
                    characters,
                )?;
                snapshot(tx, value)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_report_bytes(
        &self,
        actor: Actor,
        run: RunId,
        report: ReportReference,
        bytes: std::ops::Range<u64>,
    ) -> AppFuture<'_, Snapshot<reviewq_app::reports::ReportSourceRange>> {
        Box::pin(async move {
            if bytes.start >= bytes.end || bytes.end - bytes.start > 64 * 1024 {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| {
                let version = authorized_version(tx, &actor, &run, &report)?;
                if !version.summary.stats.complete || bytes.end > version.summary.stats.byte_length
                {
                    return Err(AppError::CursorExpired.into());
                }
                let part = read_artifact_bytes(tx, &version.artifact_id, bytes.start, bytes.end)?;
                let scalar_start = part.first_scalar.ok_or(AppError::CursorExpired)?;
                let markdown =
                    String::from_utf8(part.bytes).map_err(|_| AppError::CursorExpired)?;
                snapshot(
                    tx,
                    reviewq_app::reports::ReportSourceRange {
                        markdown,
                        scalar_start,
                    },
                )
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
                let (hash, chunk): (String, Option<Vec<u8>>) = tx.query_row(
                    "SELECT a.sha256, c.raw FROM report_versions v JOIN artifacts a USING(artifact_id)
                     LEFT JOIN artifact_chunks c ON c.artifact_id=a.artifact_id AND c.ordinal=?3
                     WHERE v.run_id=?1 AND v.receipt_id=?2 AND a.sealed=1",
                    params![run.as_str(), report.receipt_id.as_str(), ordinal],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional()?.ok_or(AppError::NotFound)?;
                if hash != report.hash.as_str() {
                    return Err(AppError::PreconditionFailed.into());
                }
                Ok(chunk)
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
                       h.reason, a.byte_length, a.total_code_points, a.line_count, a.max_line_bytes, a.complete,
                       a.original_sha256, h.reconciled_by_batch_id
                     FROM report_handoffs h JOIN artifacts a USING(artifact_id)
                     WHERE h.run_id=?1 AND h.receipt_id=?2 AND h.state='failed' AND a.sealed=1"
                )?;
                let mut rows = statement.query(params![run.as_str(), receipt.as_str()])?;
                let row = rows.next()?.ok_or(AppError::NotFound)?;
                let reference = ReportReference { receipt_id: receipt, hash: parse(text(row, 1)?)? };
                let stats = decode_stats(row, 6)?;
                let candidate = CandidateReport {
                    section: section(tx, SectionSource {
                        artifact_id: text(row, 0)?,
                        reference: reference.clone(),
                        original_sha256: optional(row, 11)?,
                        stats: &stats,
                    }, offset, characters)?,
                    reference,
                    stats,
                    turn_id: row.get(2)?,
                    call_id: row.get(3)?,
                    declaration_sha256: parse(text(row, 4)?)?,
                    reason: row.get(5)?,
                    reconciled_by_batch_id: row.get(12)?,
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
    let review = if let ActorScope::NativeSession(identity) = actor.scope() {
        let lead = lead_for_run(connection, run)?.ok_or(AppError::NotFound)?;
        if identity != lead.binding().identity() {
            return Err(AppError::NotFound.into());
        }
        lead.review_id().clone()
    } else {
        let review = connection
            .query_row(
                "SELECT review_id FROM runs WHERE run_id=?1",
                [run.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(AppError::NotFound)?;
        parse(&review)?
    };
    allow_review(
        connection,
        access,
        &review,
        &identity_for_review(connection, &review)?,
    )
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
        "WITH latest AS (
            SELECT receipt_id, CASE state
              WHEN 'pending_completion' THEN CASE WHEN artifact_id IS NULL THEN 'preparing' ELSE 'saving' END
              WHEN 'recorded' THEN 'saved' ELSE 'failed' END AS delivery, reason,
              CASE WHEN state='pending_completion' THEN 0 ELSE 2 END AS priority
            FROM report_handoffs WHERE run_id=?1 AND reconciled_by_batch_id IS NULL
            ORDER BY created_at_ms DESC, receipt_id DESC LIMIT 1
         ), failure AS (
            SELECT h.receipt_id, 'failed' AS delivery, f.reason, 1 AS priority
            FROM report_delivery_failures f
            LEFT JOIN report_handoffs h ON (h.run_id,h.lead_session_key,h.turn_id,h.call_id)=
                (f.run_id,f.lead_session_key,f.native_turn_id,f.native_call_id) AND h.state='failed'
            WHERE f.run_id=?1 AND f.resolved_by_receipt_id IS NULL
            ORDER BY f.occurred_at_ms DESC, f.notification_id DESC LIMIT 1
         )
         SELECT receipt_id, delivery, reason, priority FROM latest
         UNION ALL SELECT receipt_id, delivery, reason, priority FROM failure
         ORDER BY priority LIMIT 1",
    )?;
    let mut rows = statement.query([run.as_str()])?;
    rows.next()?
        .map(|row| {
            Ok(ReportDelivery {
                receipt_id: optional(row, 0)?,
                state: parse(text(row, 1)?)?,
                reason: row.get(2)?,
            })
        })
        .transpose()
}

pub(crate) fn authorized_version(
    connection: &Connection,
    actor: &Actor,
    run: &RunId,
    reference: &ReportReference,
) -> Result<Version, StoreError> {
    authorize_run(connection, actor, run)?;
    let version = version(connection, run, &reference.receipt_id)?;
    if version.summary.reference.hash != reference.hash {
        return Err(AppError::PreconditionFailed.into());
    }
    Ok(version)
}

pub(crate) fn version(
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
            coverage_source_sha: optional(row, 21)?,
            coverage_target_sha: optional(row, 22)?,
            verdict: parse(text(row, 8)?)?,
            semantic_index: parse(text(row, 9)?)?,
            finding_count: read_optional_count(row, 10)?,
            change_count: read_optional_count(row, 11)?,
            unanchored_finding_count: read_count(row, 23)?,
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

pub(crate) fn read_markdown(
    connection: &Connection,
    version: &Version,
) -> Result<String, StoreError> {
    let stats = &version.summary.stats;
    stats.require_markdown_budget()?;
    let range = read_artifact_bytes(connection, &version.artifact_id, 0, stats.byte_length)?;
    String::from_utf8(range.bytes).map_err(|_| AppError::Internal.into())
}

pub(crate) struct ArtifactBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) first_scalar: Option<u64>,
}

/// Copy only a byte range, using the leading chunk's indexed byte key for both coordinates.
pub(crate) fn read_artifact_bytes(
    connection: &Connection,
    artifact: &str,
    start: u64,
    end: u64,
) -> Result<ArtifactBytes, StoreError> {
    if start > end {
        return Err(AppError::Internal.into());
    }
    let mut result = Vec::with_capacity((end - start) as usize);
    if start == end {
        return Ok(ArtifactBytes {
            bytes: result,
            first_scalar: None,
        });
    }
    let mut first_scalar = None;
    let mut statement = connection.prepare_cached(
        "SELECT byte_start, scalar_start, raw FROM artifact_chunks
         WHERE artifact_id=?1 AND byte_start >= (
             SELECT byte_start FROM artifact_chunks WHERE artifact_id=?1 AND byte_start<=?2
             ORDER BY byte_start DESC LIMIT 1
         ) AND byte_start<?3 ORDER BY byte_start",
    )?;
    let mut rows = statement.query(params![artifact, start as i64, end as i64])?;
    let mut next = start;
    while let Some(row) = rows.next()? {
        let offset = read_count(row, 0)?;
        let bytes = raw(row, 2)?;
        let skip = next.checked_sub(offset).ok_or(AppError::Internal)?;
        // Only the first chunk may start before the next expected byte.
        if skip >= bytes.len() as u64 || (!result.is_empty() && skip != 0) {
            return Err(AppError::Internal.into());
        }
        if result.is_empty() {
            let chunk_scalar = read_count(row, 1)?;
            // A byte address inside an encoded scalar is not a valid block start.
            first_scalar = str::from_utf8(&bytes[..skip as usize])
                .ok()
                .and_then(|prefix| chunk_scalar.checked_add(prefix.chars().count() as u64));
        }
        let take = (end - next).min(bytes.len() as u64 - skip);
        result.extend_from_slice(&bytes[skip as usize..(skip + take) as usize]);
        next += take;
    }
    if next != end {
        return Err(AppError::Internal.into());
    }
    Ok(ArtifactBytes {
        bytes: result,
        first_scalar,
    })
}

fn section(
    connection: &Connection,
    source: SectionSource<'_>,
    offset: u64,
    characters: u32,
) -> Result<ReportSection, StoreError> {
    let total = source.stats.total_code_points;
    let (text, end) = section_text(connection, source.artifact_id, total, offset, characters)?;
    Ok(ReportSection {
        report: source.reference,
        complete: source.stats.complete,
        original_sha256: source.original_sha256,
        text,
        offset,
        next_offset: (end < total).then_some(end),
        total_code_points: total,
    })
}

fn section_text(
    connection: &Connection,
    artifact: &str,
    total: u64,
    offset: u64,
    characters: u32,
) -> Result<(String, u64), StoreError> {
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
    Ok((text, end))
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
        let (slice, _) = section_text(
            connection,
            &stored.artifact_id,
            stored.summary.stats.total_code_points,
            quote.offset(),
            quote.length() as u32,
        )?;
        if slice != quote.text().as_str() {
            return Err(AppError::PreconditionFailed.into());
        }
    }
    current_receipt(connection, run)
}
