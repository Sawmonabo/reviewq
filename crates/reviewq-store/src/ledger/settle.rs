//! Settling: the runs with an open pass or unpublished records and their facts from stored
//! session state, and the failures settling and refused calls record.

use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::ledger::{FailureRecord, PassId, SettleCandidate};
use reviewq_domain::identity::Revision;
use reviewq_domain::ledger::{FailureKind, PassState, SettleFacts};
use rusqlite::Connection;

use super::passes::{self, PASS_STATE};
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::hosts::claude::watch;
use crate::notifications::publish;
use crate::queue::{lead_for_run, parse, text};
use crate::reports::failures::{self, Noted};
use crate::workspace::advance_queue;

/// A session that is working, blocked or asking is not idle. Reads `review_active_sessions`,
/// the same evidence the stage classifier reads, which keeps a Claude session's stored state
/// until its companion is gone, so a gone companion's lead does not count as working. A
/// Claude lead whose companion is neither live nor gone is not idle either, whatever it last
/// reported, so a restart or a sleep settles nothing until it reports or counts as gone. A
/// lead with no stored activity is unknown, such as a Codex thread running in
/// another process, and its run is neither settled nor updated. A lead not yet seen starting a
/// turn for the open pass is not idle either: it has not begun the work the pass waits for. A
/// lead's own record write makes a stored idle or failed state unknown, because the call
/// proves its turn is running. A child counts as idle unless it shows work, because a child's
/// last turn can stay unknown long after the child ended.
///
/// `candidates` leaves out a run whose unresolved failure of the kind settling would record
/// (unfinished review for its pass, failed update without one) was recorded at its current
/// ledger sequence, so it is not reworked until a record write or closing call changes it.
const BUSY: &str = "(s.host_activity IN ('working','blocked') OR s.host_questions=1)";

/// Recovery states that keep a run from settling.
const RECOVERING: &str = "('waiting','queued','started','uncertain')";

pub(super) fn candidates(db: &Connection) -> Result<Vec<SettleCandidate>, StoreError> {
    let sql = format!(
        "WITH candidates(run_id) AS (
           SELECT run_id FROM ledger_runs WHERE sequence>published_sequence
           UNION SELECT run_id FROM review_passes WHERE state<>'closed')
         SELECT x.run_id, coalesce(l.sequence, 0), p.pass_id, {PASS_STATE},
           (SELECT count(*) FROM ledger_findings f WHERE f.run_id=x.run_id AND f.active=1),
           (p.pass_id IS NULL OR p.lead_started_at_ms IS NOT NULL)
             AND EXISTS(SELECT 1 FROM sessions s WHERE s.session_key=r.lead_session_key
               AND s.host_activity IS NOT NULL)
             AND NOT EXISTS(SELECT 1 FROM review_active_sessions s WHERE s.run_id=x.run_id
               AND s.parent_session_key IS NULL AND {BUSY})
             AND NOT EXISTS(SELECT 1 FROM claude_companions c WHERE c.session_key=r.lead_session_key
               AND {unknown}),
           NOT EXISTS(SELECT 1 FROM review_active_sessions s WHERE s.run_id=x.run_id
             AND s.parent_session_key IS NOT NULL AND {BUSY}),
           EXISTS(SELECT 1 FROM recovery_episodes e WHERE e.run_id=x.run_id AND e.state IN {RECOVERING}),
           EXISTS(SELECT 1 FROM report_heads h WHERE h.run_id=x.run_id),
           coalesce(l.sequence>l.published_sequence, 0),
           CASE WHEN p.kind='full_review' THEN coalesce((SELECT group_concat(DISTINCT n.section)
             FROM ledger_notes n WHERE n.run_id=x.run_id AND n.present=1), '') END
         FROM candidates x JOIN runs r ON r.run_id=x.run_id
         JOIN reviews v ON v.review_id=r.review_id AND v.state='open'
         LEFT JOIN ledger_runs l ON l.run_id=x.run_id
         LEFT JOIN review_passes p ON p.run_id=x.run_id AND p.state<>'closed'
         WHERE NOT EXISTS(SELECT 1 FROM report_delivery_failures d WHERE d.run_id=x.run_id
             AND coalesce(d.pass_id, 0)=coalesce(p.pass_id, 0)
             AND d.kind=CASE WHEN p.pass_id IS NULL THEN 'update_failed' ELSE 'unfinished_review' END
             AND d.resolved_at_ms IS NULL AND d.ledger_sequence=coalesce(l.sequence, 0))
         ORDER BY x.run_id",
        unknown = watch::unknown("c.seen_at_ms"),
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query([])?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next()? {
        let pass: Option<i64> = row.get(2)?;
        let state: Option<String> = row.get(3)?;
        candidates.push(SettleCandidate {
            run: parse(text(row, 0)?)?,
            sequence: Revision::try_from(row.get::<_, i64>(1)?).map_err(|_| AppError::Internal)?,
            pass: pass
                .map(PassId::try_from)
                .transpose()
                .map_err(|_| AppError::Internal)?,
            active_findings: u32::try_from(row.get::<_, i64>(4)?)
                .map_err(|_| AppError::Internal)?,
            full_review_notes: row
                .get::<_, Option<String>>(10)?
                .map(|sections| {
                    sections
                        .split(',')
                        .filter(|section| !section.is_empty())
                        .map(parse)
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
            facts: SettleFacts {
                lead_idle: row.get(5)?,
                children_idle: row.get(6)?,
                recovery_pending: row.get(7)?,
                pass: state
                    .map(|state| PassState::from_label(&state).ok_or(AppError::Internal))
                    .transpose()?,
                has_version: row.get(8)?,
                unpublished: row.get(9)?,
            },
        });
    }
    Ok(candidates)
}

/// Records a failure once per run, pass and kind. An unfinished selected-thread pass lets
/// its batch's slot go, so a successor batch takes over its records.
pub(super) fn record_failure(
    tx: &mut WriteTx<'_>,
    failure: &FailureRecord,
) -> Result<(), StoreError> {
    let lead = lead_for_run(&tx.sql, &failure.run)?.ok_or(AppError::NotFound)?;
    let time = now_ms()?;
    let noted = failures::note(&tx.sql, &lead, failure, time)?;
    if failure.kind == FailureKind::UnfinishedReview
        && let Some(row) = passes::current(&tx.sql, failure.run.as_str())?
        && Some(row.id) == failure.pass
    {
        passes::release_batch(&tx.sql, &row)?;
    }
    if let Noted::First(notification) = noted {
        publish(tx, notification.as_slice())?;
    }
    advance_queue(&tx.sql)?;
    tx.emit(&[
        ResourceKey::Queue,
        ResourceKey::Review(Some(lead.review_id().clone())),
        ResourceKey::Run(Some(lead.run_id().clone())),
        ResourceKey::Report(Some(lead.run_id().clone())),
    ])?;
    Ok(())
}
