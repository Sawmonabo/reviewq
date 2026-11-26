//! Review passes: opened inside the launch confirmation, the batch delivery and the first
//! record write, held by `needs_input`, reopened by the lead's next turn, ended by a
//! successor batch or archiving. Also the stop gate, the read-back and the hook reports.

use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::ledger::{
    CallBinding, FindingLine, FrozenBatch, GateAnswer, GateRequest, HoldReceipt, HookSession,
    OpenPass, PassId, ReadBack,
};
use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_domain::identity::{NativeTurnId, ReviewId, RunId};
use reviewq_domain::ledger::{
    CallerRole, GateDecision, GateFacts, PassEvent, PassKind, PassState, Tool, gate, may_call,
    opens_pass,
};
use reviewq_domain::session::HostProvider;
use rusqlite::{Connection, OptionalExtension, params};

use super::rows::LINE_COLUMNS;
use super::{active_findings, caller, calls, rows, sequence, unpublished};
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::followups::completion;
use crate::notifications::{insert_notification, publish};
use crate::queue::{parse, text};
use crate::workspace::{advance_queue, provider_label};

/// A pass's state as the lead sees it: a held pass reads as open once the lead's next input
/// is newer than the hold, a started ReviewQ message or a new Claude turn. Writes that see a
/// reopened pass store it (`reopen`). Uses the alias `p` for `review_passes`.
pub(super) const PASS_STATE: &str = "CASE WHEN p.state='waiting' AND (
      EXISTS(SELECT 1 FROM runs r JOIN messages m ON m.run_id=r.run_id AND m.session_key=r.lead_session_key
             WHERE r.run_id=p.run_id AND m.state='started' AND m.updated_at_ms>p.held_at_ms)
      OR EXISTS(SELECT 1 FROM runs r JOIN claude_companions c ON c.session_key=r.lead_session_key
             WHERE r.run_id=p.run_id AND p.held_turn_id IS NOT NULL AND c.turn_id<>p.held_turn_id))
    THEN 'open' ELSE p.state END";

/// The hold reason that frees an executing batch's slot once the lead is idle.
const INPUT_REQUIRED: &str = "followup_native_input_required";

/// What opened a pass.
pub(super) enum Opener<'a> {
    Record(&'a str),
}

/// The run's pass that is not closed, as `PASS_STATE` reads it.
pub(super) struct PassRow {
    pub(super) id: PassId,
    pub(super) kind: PassKind,
    pub(super) state: PassState,
    /// The ledger sequence when it opened.
    pub(super) opened_sequence: i64,
    /// When it opened, in Unix milliseconds.
    pub(super) opened_at_ms: i64,
    pub(super) batch: Option<String>,
    gate_turn: Option<String>,
    gate_blocks: u8,
    /// The question `needs_input` stored, read only while `state` is `Waiting`: a held pass
    /// that `PASS_STATE` reads as open may still store the question it no longer waits on.
    question: Option<String>,
}

pub(super) fn current(db: &Connection, run: &str) -> Result<Option<PassRow>, StoreError> {
    let sql = format!(
        "SELECT p.pass_id, p.kind, {PASS_STATE}, p.opened_sequence, p.batch_id, p.gate_turn_id,
           p.gate_blocks, p.opened_at_ms, p.hold_reason
         FROM review_passes p WHERE p.run_id=?1 AND p.state<>'closed'"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query([run])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let state = PassState::from_label(text(row, 2)?).ok_or(AppError::Internal)?;
    Ok(Some(PassRow {
        id: PassId::try_from(row.get::<_, i64>(0)?).map_err(|_| AppError::Internal)?,
        kind: PassKind::from_label(text(row, 1)?).ok_or(AppError::Internal)?,
        state,
        opened_sequence: row.get(3)?,
        batch: row.get(4)?,
        gate_turn: row.get(5)?,
        gate_blocks: u8::try_from(row.get::<_, i64>(6)?).map_err(|_| AppError::Internal)?,
        opened_at_ms: row.get(7)?,
        question: match state {
            PassState::Waiting => row.get(8)?,
            PassState::Open | PassState::Closed => None,
        },
    }))
}

pub(super) fn current_id(db: &Connection, run: &str) -> Result<Option<i64>, StoreError> {
    Ok(db
        .prepare_cached("SELECT pass_id FROM review_passes WHERE run_id=?1 AND state<>'closed'")?
        .query_row([run], |row| row.get(0))
        .optional()?)
}

/// The run's pass that is not closed, with its frozen batch.
pub(super) fn open_pass(db: &Connection, run: &str) -> Result<Option<OpenPass>, StoreError> {
    current(db, run)?.map(|row| describe(db, &row)).transpose()
}

pub(super) fn describe(db: &Connection, row: &PassRow) -> Result<OpenPass, StoreError> {
    let batch = match &row.batch {
        Some(batch) => {
            let manifest = completion::frozen(db, batch)?;
            let mut discussion_ids: Vec<String> = Vec::new();
            for member in manifest.observations {
                if !discussion_ids.contains(&member.discussion_id) {
                    discussion_ids.push(member.discussion_id);
                }
            }
            Some(FrozenBatch {
                id: manifest.batch_id,
                checked_sha: parse(&manifest.source_sha)?,
                target_sha: manifest.target_sha.as_deref().map(parse).transpose()?,
                discussion_ids,
            })
        }
        None => None,
    };
    Ok(OpenPass {
        id: row.id,
        kind: row.kind,
        state: row.state,
        batch,
        question: row.question.clone(),
    })
}

/// Opens a pass unless the run already has one that is not closed.
pub(super) fn open(
    db: &Connection,
    run: &str,
    kind: PassKind,
    opener: Opener<'_>,
    opened_sequence: u64,
    time: i64,
) -> Result<(), StoreError> {
    let Opener::Record(call) = opener;
    db.prepare_cached(
        "INSERT INTO review_passes(run_id, kind, state, opened_by, call_id, opened_sequence,
           opened_at_ms, lead_started_at_ms)
         VALUES(?1, ?2, 'open', 'record', ?3, ?4, ?5, ?5) ON CONFLICT DO NOTHING",
    )?
    .execute(params![
        run,
        kind.as_str(),
        call,
        i64::try_from(opened_sequence).map_err(|_| AppError::Internal)?,
        time
    ])?;
    Ok(())
}

/// The open time when the lead of the run in `run` is already stored as working or blocked,
/// else NULL: a lead seen in its turn before the pass opened has started the pass. A lead
/// still idle, as registration left it, has not. `time` names the open time's parameter.
fn started_at_open(run: &str, time: &str) -> String {
    format!(
        "(SELECT {time} FROM runs r JOIN sessions s ON s.session_key=r.lead_session_key
          WHERE r.run_id={run} AND s.host_activity IN ('working','blocked'))"
    )
}

/// Opens the full-review pass of a launch that just became confirmed. Inside the launch's
/// confirmation transaction; opens nothing when the launch is not confirmed or bound to a run.
pub(crate) fn open_for_launch(db: &Connection, launch: &str, time: i64) -> Result<(), StoreError> {
    let Some(kind) = opens_pass(PassEvent::ReviewStarted) else {
        return Ok(());
    };
    let started = started_at_open("l.run_id", "?3");
    db.prepare_cached(&format!(
        "INSERT INTO review_passes(run_id, kind, state, opened_by, launch_id, opened_sequence,
           opened_at_ms, lead_started_at_ms)
         SELECT l.run_id, ?2, 'open', 'launch', l.id,
           coalesce((SELECT sequence FROM ledger_runs s WHERE s.run_id=l.run_id), 0), ?3, {started}
         FROM launch_attempts l WHERE l.id=?1 AND l.state='confirmed' AND l.run_id IS NOT NULL
         ON CONFLICT DO NOTHING"
    ))?
    .execute(params![launch, kind.as_str(), time])?;
    Ok(())
}

/// Opens the selected-thread pass of a batch the lead just started, and ends the pass of the
/// predecessor batch whose records it takes over. Inside the delivery's transaction.
pub(crate) fn open_for_batch(
    db: &Connection,
    review: &ReviewId,
    batch: &str,
    time: i64,
) -> Result<(), StoreError> {
    let Some(kind) = opens_pass(PassEvent::BatchDelivered) else {
        return Ok(());
    };
    db.prepare_cached(
        "UPDATE review_passes SET state='closed', closed_by='successor', closed_at_ms=?3
         WHERE run_id=(SELECT run_id FROM followup_batches WHERE review_id=?1 AND batch_id=?2)
           AND kind='selected_threads' AND state<>'closed'",
    )?
    .execute(params![review.as_str(), batch, time])?;
    let started = started_at_open("b.run_id", "?4");
    db.prepare_cached(&format!(
        "INSERT INTO review_passes(run_id, kind, state, opened_by, review_id, batch_id,
           opened_sequence, opened_at_ms, lead_started_at_ms)
         SELECT b.run_id, ?3, 'open', 'batch', b.review_id, b.batch_id,
           coalesce((SELECT sequence FROM ledger_runs s WHERE s.run_id=b.run_id), 0), ?4, {started}
         FROM followup_batches b WHERE b.review_id=?1 AND b.batch_id=?2 AND b.state='executing'
         ON CONFLICT DO NOTHING"
    ))?
    .execute(params![review.as_str(), batch, kind.as_str(), time])?;
    Ok(())
}

/// Ends every pass of a review that is no longer open. Inside the refresh that archived it.
pub(crate) fn end_for_review(
    db: &Connection,
    review: &ReviewId,
    time: i64,
) -> Result<(), StoreError> {
    db.prepare_cached(
        "UPDATE review_passes SET state='closed', closed_by='archive', closed_at_ms=?2
         WHERE state<>'closed' AND run_id IN (SELECT run_id FROM runs WHERE review_id=?1)",
    )?
    .execute(params![review.as_str(), time])?;
    Ok(())
}

/// A lead call or a lead stop request on `run`: the lead has started a turn for the open
/// pass, and a held pass reopens when this is a turn other than the one that held it.
pub(super) fn lead_seen(
    db: &Connection,
    run: &str,
    turn: Option<&NativeTurnId>,
    time: i64,
) -> Result<(), StoreError> {
    mark_started(db, run, time)?;
    reopen(db, run, turn, false)
}

/// The run's lead started a turn, as its stored activity shows: the open pass records that
/// its lead started, and a held pass reopens. Inside the transaction that stores the lead's
/// activity. A hold records its lead as working first, so the activity that follows the
/// holding turn is a change only once the lead was seen out of that turn.
pub(crate) fn lead_started(db: &Connection, run: &str, time: i64) -> Result<(), StoreError> {
    mark_started(db, run, time)?;
    reopen(db, run, None, true)
}

/// The open pass records that its lead started, once: the lead started a turn, called, or
/// failed. Settling waits for it.
pub(crate) fn mark_started(db: &Connection, run: &str, time: i64) -> Result<(), StoreError> {
    db.prepare_cached(
        "UPDATE review_passes SET lead_started_at_ms=?2
         WHERE run_id=?1 AND state<>'closed' AND lead_started_at_ms IS NULL",
    )?
    .execute(params![run, time])?;
    Ok(())
}

/// Stores the reopening of a held pass: the lead's stored activity started a new turn
/// (`input`), its next input is newer than the hold, or it calls in a turn other than the one
/// that held it. A batch still waiting for the lead keeps its slot again.
pub(super) fn reopen(
    db: &Connection,
    run: &str,
    turn: Option<&NativeTurnId>,
    input: bool,
) -> Result<(), StoreError> {
    let sql = format!(
        "UPDATE review_passes AS p SET state='open', held_turn_id=NULL, held_at_ms=NULL,
           hold_reason=NULL
         WHERE p.run_id=?1 AND p.state<>'closed' AND p.state='waiting'
           AND (?3 OR (?2 IS NOT NULL AND p.held_turn_id IS NOT NULL AND ?2<>p.held_turn_id)
             OR {PASS_STATE}='open')
         RETURNING review_id, batch_id"
    );
    let reopened = db
        .prepare_cached(&sql)?
        .query_row(params![run, turn.map(|turn| turn.as_str()), input], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .optional()?;
    if let Some((Some(review), Some(batch))) = reopened {
        db.prepare_cached(
            "UPDATE followup_batches SET hold_reason=NULL WHERE review_id=?1 AND batch_id=?2
               AND state='executing' AND holds_slot=1 AND hold_reason=?3",
        )?
        .execute(params![review, batch, INPUT_REQUIRED])?;
    }
    Ok(())
}

/// Lets an executing batch's slot go once the lead is idle, so a successor batch can take
/// over its records.
pub(super) fn release_batch(db: &Connection, row: &PassRow) -> Result<(), StoreError> {
    if let Some(batch) = &row.batch {
        db.prepare_cached(
            "UPDATE followup_batches SET hold_reason=?2 WHERE batch_id=?1 AND state='executing'
               AND authority_verified=1 AND hold_reason IS NULL",
        )?
        .execute(params![batch, INPUT_REQUIRED])?;
    }
    Ok(())
}

/// `needs_input`: holds the open pass until the lead's next turn and raises the question once
/// per hold. The call proves its turn is running, so the lead is stored as working unless it
/// already is or is blocked; the host's later sight of the same turn is then no change and
/// cannot reopen the hold. A repeat while the pass still waits keeps the hold and updates its
/// reason.
pub(super) fn hold(
    tx: &mut WriteTx<'_>,
    binding: &CallBinding,
    reason: &str,
) -> Result<Option<HoldReceipt>, StoreError> {
    let db = &tx.sql;
    let caller = caller::resolve(db, binding)?;
    if !may_call(Tool::NeedsInput, caller.role()) {
        return Err(AppError::Forbidden.into());
    }
    if let Some((stored, _)) = calls::replay(db, &caller, Tool::NeedsInput, RUN_TARGET)? {
        return calls::hold_from(&stored);
    }
    let lead = &caller.lead;
    let run = lead.run_id().as_str();
    let time = now_ms()?;
    lead_seen(db, run, caller.turn.as_ref(), time)?;
    let working = lead_working(db, run)?;
    let receipt = match current(db, run)? {
        None => None,
        Some(row) => {
            let held_at: i64 = db
                .prepare_cached(
                    "UPDATE review_passes SET hold_reason=?4,
                       held_turn_id=CASE WHEN state='waiting' THEN coalesce(held_turn_id, ?2) ELSE ?2 END,
                       held_at_ms=CASE WHEN state='waiting' THEN held_at_ms ELSE ?3 END,
                       state='waiting'
                     WHERE pass_id=?1 RETURNING held_at_ms",
                )?
                .query_row(
                    params![
                        row.id.get(),
                        caller.turn.as_ref().map(|turn| turn.as_str()),
                        time,
                        reason
                    ],
                    |row| row.get(0),
                )?;
            release_batch(db, &row)?;
            Some((
                HoldReceipt {
                    pass: row.kind,
                    unpublished: unpublished(db, run)?,
                },
                format!("needs_input:{}:{held_at}", row.id.get()),
            ))
        }
    };
    let (receipt, occurrence) = receipt.unzip();
    let result = calls::hold_json(receipt.as_ref());
    calls::store(
        db,
        &caller,
        Tool::NeedsInput,
        RUN_TARGET,
        sequence(db, run)?,
        &result,
        time,
    )?;
    if let Some(occurrence) = occurrence {
        let notification = insert_notification(
            db,
            Observation {
                source: Source::Report,
                occurrence_id: occurrence.clone(),
                kind: Kind::AgentQuestion,
                review: lead.review_id().clone(),
                run: Some(lead.run_id().clone()),
                session: Some(lead.session_key().clone()),
                event_ref: Some(occurrence),
                label: "Review needs input".to_owned(),
                occurred_at_ms: time,
                desktop_episode: None,
            },
        )?;
        let (review, run) = (lead.review_id().clone(), lead.run_id().clone());
        publish(tx, notification.as_slice())?;
        advance_queue(&tx.sql)?;
        tx.emit(&[
            ResourceKey::Queue,
            ResourceKey::Followup(Some(review.clone())),
            ResourceKey::Review(Some(review)),
            ResourceKey::Run(Some(run)),
        ])?;
    } else if working {
        advance_queue(&tx.sql)?;
        tx.emit(&[
            ResourceKey::Queue,
            ResourceKey::Run(Some(lead.run_id().clone())),
        ])?;
    }
    Ok(receipt)
}

/// Stores the run's lead as working, through the one activity writer, unless it is already
/// working or blocked. Returns whether the stored activity changed.
fn lead_working(db: &Connection, run: &str) -> Result<bool, StoreError> {
    let lead: Option<(String, bool)> = db
        .prepare_cached(
            "SELECT s.session_key, s.host_questions FROM runs r
             JOIN sessions s ON s.session_key=r.lead_session_key
             WHERE r.run_id=?1 AND coalesce(s.host_activity, '') NOT IN ('working','blocked')",
        )?
        .query_row([run], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    match lead {
        Some((session, questions)) => {
            crate::hosts::record_activity(db, &session, Some("working"), questions)
        }
        None => Ok(false),
    }
}

/// The target `needs_input` and `finish_review` store: the run, since neither names a record.
pub(super) const RUN_TARGET: &str = "run";

/// Answers one stop request and notes that the provider's stop hook reported.
pub(super) fn answer(db: &Connection, request: &GateRequest) -> Result<GateAnswer, StoreError> {
    let provider = match request.session {
        HookSession::Claude(_) => HostProvider::Claude,
        HookSession::Codex(_) => HostProvider::Codex,
    };
    let time = now_ms()?;
    db.prepare_cached(
        "INSERT INTO hook_reports(provider, event, reported_at_ms) VALUES(?1, 'stop', ?2)
         ON CONFLICT(provider, event) DO UPDATE SET reported_at_ms=excluded.reported_at_ms
         WHERE reported_at_ms < excluded.reported_at_ms - 3600000",
    )?
    .execute(params![provider_label(provider), time])?;
    let release = GateAnswer {
        decision: GateDecision::Release { blocks: 0 },
        pass: None,
        active_findings: 0,
    };
    let Some((run, session, true)) = caller::hook(db, &request.session)? else {
        return Ok(release);
    };
    let run = run.as_str();
    lead_seen(db, run, request.turn.as_ref(), time)?;
    let row = current(db, run)?;
    let turn = request.turn.as_ref().map(|turn| turn.as_str());
    let blocks = row
        .as_ref()
        .filter(|row| row.gate_turn.as_deref() == turn)
        .map_or(0, |row| row.gate_blocks);
    let pending_question: bool = db
        .prepare_cached(
            "SELECT s.host_questions=1 OR s.host_activity IS 'blocked'
               OR EXISTS(SELECT 1 FROM claude_companions c WHERE c.session_key=s.session_key
                    AND c.permission_turn_id IS NOT NULL)
               OR EXISTS(SELECT 1 FROM native_async_questions q WHERE q.session_key=s.session_key
                    AND q.state IN ('pending','submitting','uncertain'))
             FROM sessions s WHERE s.session_key=?1",
        )?
        .query_row([session.as_str()], |row| row.get(0))?;
    let decision = gate(GateFacts {
        role: CallerRole::Lead,
        pass: row.as_ref().map(|row| row.state),
        pending_question,
        stop_hook_active: request.stop_hook_active,
        blocks,
    });
    let pass = match &row {
        Some(row) => {
            let (GateDecision::Release { blocks } | GateDecision::Block { blocks }) = decision;
            db.prepare_cached(
                "UPDATE review_passes SET gate_turn_id=?2, gate_blocks=?3 WHERE pass_id=?1",
            )?
            .execute(params![row.id.get(), turn, blocks])?;
            Some(describe(db, row)?)
        }
        None => None,
    };
    Ok(GateAnswer {
        decision,
        pass,
        active_findings: active_findings(db, run)?,
    })
}

/// The open pass and every active finding, newest change first, for a run's lead session.
pub(super) fn read_back(
    db: &Connection,
    session: &HookSession,
) -> Result<Option<ReadBack>, StoreError> {
    let Some((run, _, true)) = caller::hook(db, session)? else {
        return Ok(None);
    };
    Ok(Some(ReadBack {
        pass: open_pass(db, run.as_str())?,
        findings: lines(db, &run, "f.sequence DESC")?,
    }))
}

/// One line per active finding of `run`, in `order` over `ledger_findings f`.
pub(super) fn lines(
    db: &Connection,
    run: &RunId,
    order: &str,
) -> Result<Vec<FindingLine>, StoreError> {
    let sql = format!(
        "SELECT {LINE_COLUMNS} FROM ledger_findings f
         JOIN ledger_changes c ON c.run_id=f.run_id AND c.sequence=f.sequence
         WHERE f.run_id=?1 AND f.active=1 ORDER BY {order}"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut found = statement.query([run.as_str()])?;
    let mut lines = Vec::new();
    while let Some(row) = found.next()? {
        lines.push(rows::line(row)?);
    }
    Ok(lines)
}

pub(super) fn last_stop_hook_report(
    db: &Connection,
    provider: HostProvider,
) -> Result<Option<i64>, StoreError> {
    Ok(db
        .prepare_cached(
            "SELECT reported_at_ms FROM hook_reports WHERE provider=?1 AND event='stop'",
        )?
        .query_row([provider_label(provider)], |row| row.get(0))
        .optional()?)
}
