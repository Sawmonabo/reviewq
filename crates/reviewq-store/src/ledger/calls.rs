//! The stored result of each accepted ledger call, keyed by provider, native session and call
//! ID, so a repeat of the call returns it unchanged.

use reviewq_app::access::AppError;
use reviewq_app::followups::NextAction;
use reviewq_app::ledger::{
    Caller, ClosingSummary, HoldReceipt, NoteKey, RecordReceipt, SubmitReceipt,
};
use reviewq_app::reports::Assessment;
use reviewq_domain::identity::Revision;
use reviewq_domain::ledger::{FindingId, PassKind, Tool};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::rows::{finding_id, from_json, note_key, note_title, revision};
use crate::StoreError;
use crate::queue::parse;
use crate::workspace::provider_label;

/// The result stored for `caller`'s call, or `None` for a new call. `IdempotencyConflict`
/// when the same call made another kind of call or named another record.
pub(super) fn replay(
    db: &Connection,
    caller: &Caller,
    tool: Tool,
    target: &str,
) -> Result<Option<(Value, Revision)>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT run_id, tool, target, result, sequence FROM ledger_calls
         WHERE provider=?1 AND native_session=?2 AND call_id=?3",
    )?;
    let stored = statement
        .query_row(
            params![
                provider_label(caller.provider()),
                caller.native_session().as_str(),
                caller.call_id
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((run, stored_tool, stored_target, result, sequence)) = stored else {
        return Ok(None);
    };
    if run != caller.lead.run_id().as_str()
        || stored_tool != tool.as_str()
        || stored_target != target
    {
        return Err(AppError::IdempotencyConflict.into());
    }
    let sequence = Revision::try_from(sequence).map_err(|_| AppError::Internal)?;
    Ok(Some((from_json(&result)?, sequence)))
}

/// The target of a `save_finding` create, which names no record; its result holds the ID.
pub(super) const NEW_FINDING: &str = "finding:new";

pub(super) fn finding_target(id: FindingId) -> String {
    format!("finding:{id}")
}

pub(super) fn note_target(key: &NoteKey) -> String {
    format!("note:{}:{}", key.section(), note_title(key))
}

/// The ordinal of the last accepted ledger call of the run `run`, an SQL expression, or 0
/// before its first.
pub(super) fn last_ordinal(run: &str) -> String {
    format!("coalesce((SELECT max(o.ordinal) FROM ledger_calls o WHERE o.run_id={run}), 0)")
}

/// Stores the result of `caller`'s accepted call at the run's ledger sequence, as the run's
/// next call in order.
pub(super) fn store(
    db: &Connection,
    caller: &Caller,
    tool: Tool,
    target: &str,
    sequence: Revision,
    result: &Value,
) -> Result<(), StoreError> {
    db.prepare_cached(&format!(
        "INSERT INTO ledger_calls(provider, native_session, call_id, run_id, tool, target,
           native_turn_id, sequence, ordinal, result)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, {} + 1, ?9)",
        last_ordinal("?4")
    ))?
    .execute(params![
        provider_label(caller.provider()),
        caller.native_session().as_str(),
        caller.call_id,
        caller.lead.run_id().as_str(),
        tool.as_str(),
        target,
        caller.turn.as_ref().map(|turn| turn.as_str()),
        sequence.as_sqlite_integer(),
        result.to_string(),
    ])?;
    Ok(())
}

/// The lead's last accepted closing call on `run`: its stored result, the ledger sequence it
/// closed at and its ordinal. Every closing call answered with success is stored, including
/// one answered with the current version.
pub(super) fn last_closing(
    db: &Connection,
    run: &str,
) -> Result<Option<(String, i64, i64)>, StoreError> {
    Ok(db
        .prepare_cached(
            "SELECT result, sequence, ordinal FROM ledger_calls
             WHERE run_id=?1 AND tool='submit_review' ORDER BY ordinal DESC LIMIT 1",
        )?
        .query_row([run], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .optional()?)
}

pub(super) fn receipt_json(receipt: &RecordReceipt) -> Value {
    match receipt {
        RecordReceipt::Finding {
            id,
            revision,
            unpublished,
        } => json!({"id": id.number(), "revision": revision.get(), "unpublished": unpublished}),
        RecordReceipt::Note {
            key,
            revision,
            unpublished,
        } => json!({"section": key.section().to_string(), "title": note_title(key),
            "revision": revision.get(), "unpublished": unpublished}),
    }
}

pub(super) fn receipt_from(value: &Value) -> Result<RecordReceipt, StoreError> {
    let revision = revision(integer(&value["revision"])?)?;
    let unpublished = count(&value["unpublished"])?;
    Ok(match value.get("id") {
        Some(id) => RecordReceipt::Finding {
            id: finding_id(integer(id)?)?,
            revision,
            unpublished,
        },
        None => RecordReceipt::Note {
            key: note_key(string(&value["section"])?, string(&value["title"])?)?,
            revision,
            unpublished,
        },
    })
}

pub(super) fn hold_json(receipt: Option<&HoldReceipt>) -> Value {
    receipt.map_or(
        Value::Null,
        |receipt| json!({"pass": receipt.pass.as_str(), "unpublished": receipt.unpublished}),
    )
}

pub(super) fn hold_from(value: &Value) -> Result<Option<HoldReceipt>, StoreError> {
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(HoldReceipt {
        pass: PassKind::from_label(string(&value["pass"])?).ok_or(AppError::Internal)?,
        unpublished: count(&value["unpublished"])?,
    }))
}

/// A closing acknowledgement with the assessments it closed a selected-thread pass with.
pub(super) fn submit_json(ack: &SubmitReceipt, assessments: &[Assessment]) -> Value {
    let summary = &ack.summary;
    let ids = |ids: &[FindingId]| ids.iter().map(|id| id.number()).collect::<Vec<_>>();
    json!({
        "receipt": ack.receipt.as_str(),
        "sha256": ack.sha256.as_str(),
        "result": summary.result.to_string(),
        "new": ids(&summary.new),
        "updated": ids(&summary.updated),
        "retired": ids(&summary.retired),
        "carried": summary.carried,
        "bySubagents": ids(&summary.by_subagents),
        "assessments": assessments.iter().map(|assessment| json!({
            "discussionId": assessment.discussion_id,
            "conclusion": assessment.conclusion.to_string(),
            "reason": assessment.reason,
            "findingIds": assessment.finding_ids,
            "nextAction": assessment.next_action.to_string(),
        })).collect::<Vec<_>>(),
    })
}

pub(super) fn submit_from(value: &Value) -> Result<(SubmitReceipt, Vec<Assessment>), StoreError> {
    let ids = |value: &Value| -> Result<Vec<FindingId>, StoreError> {
        list(value)?
            .iter()
            .map(|id| finding_id(integer(id)?))
            .collect()
    };
    let ack = SubmitReceipt {
        receipt: parse(string(&value["receipt"])?)?,
        sha256: parse(string(&value["sha256"])?)?,
        summary: ClosingSummary {
            result: parse(string(&value["result"])?)?,
            new: ids(&value["new"])?,
            updated: ids(&value["updated"])?,
            retired: ids(&value["retired"])?,
            carried: count(&value["carried"])?,
            by_subagents: ids(&value["bySubagents"])?,
        },
    };
    let assessments = list(&value["assessments"])?
        .iter()
        .map(|item| {
            Ok(Assessment {
                discussion_id: string(&item["discussionId"])?.to_owned(),
                conclusion: parse(string(&item["conclusion"])?)?,
                reason: string(&item["reason"])?.to_owned(),
                finding_ids: list(&item["findingIds"])?
                    .iter()
                    .map(|id| Ok(string(id)?.to_owned()))
                    .collect::<Result<_, StoreError>>()?,
                next_action: parse::<NextAction>(string(&item["nextAction"])?)?,
            })
        })
        .collect::<Result<_, StoreError>>()?;
    Ok((ack, assessments))
}

fn integer(value: &Value) -> Result<i64, StoreError> {
    value.as_i64().ok_or_else(|| AppError::Internal.into())
}

fn count(value: &Value) -> Result<u32, StoreError> {
    u32::try_from(integer(value)?).map_err(|_| AppError::Internal.into())
}

fn string(value: &Value) -> Result<&str, StoreError> {
    value.as_str().ok_or_else(|| AppError::Internal.into())
}

fn list(value: &Value) -> Result<&Vec<Value>, StoreError> {
    value.as_array().ok_or_else(|| AppError::Internal.into())
}
