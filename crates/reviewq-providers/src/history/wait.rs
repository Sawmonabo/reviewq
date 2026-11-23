use std::io::Read;
use std::ops::Range;

use reviewq_app::history::VisiblePartKind;
use serde::Deserialize;
use serde_json::json;

use super::file::Lines;
use super::records::{Part, Record, Text};

#[cfg(test)]
mod tests;

pub(super) const PROOF_BYTES: u64 = 2 * 1024 * 1024;
const NEARBY_BYTES: u64 = 16 * 1024;
const NEARBY_RECORDS: usize = 8;

#[derive(Deserialize)]
struct Frame<P> {
    #[serde(rename = "type")]
    kind: String,
    payload: P,
}

#[derive(Deserialize)]
struct Marker {
    #[serde(rename = "type")]
    kind: Option<String>,
    thread_id: Option<String>,
    session_id: Option<String>,
    turn_id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    namespace: Option<String>,
    internal_chat_message_metadata_passthrough: Option<TurnMarker>,
}

#[derive(Deserialize)]
struct TurnMarker {
    turn_id: String,
}

#[derive(Deserialize)]
struct Invocation {
    arguments: String,
}

#[derive(Deserialize)]
struct Arguments {
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct Output {
    output: String,
}

#[derive(Deserialize)]
struct Outcome {
    message: Option<String>,
    timed_out: bool,
}

#[derive(Clone, Copy)]
enum Side {
    Before,
    After,
}

struct Identity<'a> {
    thread: &'a str,
    turn: &'a str,
    call: &'a str,
}

pub(super) fn attach(
    lines: &mut Lines<'_>,
    thread: &str,
    span: Range<u64>,
    complete: u64,
    remaining: &mut u64,
    record: &mut Record,
) {
    let Some(turn) = &record.turn else { return };
    let identity = Identity {
        thread,
        turn: turn.as_str(),
        call: record.id.as_str(),
    };
    let Some(arguments) = related(
        lines,
        &identity,
        span.start,
        complete,
        remaining,
        Side::Before,
    )
    .and_then(|line| serde_json::from_slice::<Frame<Invocation>>(&line).ok())
    .and_then(|frame| serde_json::from_str::<Arguments>(&frame.payload.arguments).ok()) else {
        return;
    };
    let outcome = related(lines, &identity, span.end, complete, remaining, Side::After)
        .and_then(|line| serde_json::from_slice::<Frame<Output>>(&line).ok())
        .and_then(|frame| serde_json::from_str::<Outcome>(&frame.payload.output).ok());
    let Some(call) = record.parts.first_mut() else {
        return;
    };
    let VisiblePartKind::ToolCall { state, .. } = call.kind else {
        return;
    };
    if let Some(timeout) = arguments.timeout_ms {
        call.body = detail(json!({"timeout_ms": timeout}));
    }
    if let Some(outcome) = outcome {
        // Only publish the provider's fixed wait messages. Discard any other
        // text from the function output, even after proving its call identity.
        let expected = if outcome.timed_out {
            "Wait timed out."
        } else {
            "Wait completed."
        };
        let body = if outcome.message.as_deref() == Some(expected) {
            json!({"message": expected, "timed_out": outcome.timed_out})
        } else {
            json!({"timed_out": outcome.timed_out})
        };
        record.parts.push(Part {
            index: 1,
            kind: VisiblePartKind::ToolResult {
                call: record.id.clone(),
                state,
            },
            body: detail(body),
        });
    }
}

fn detail(value: serde_json::Value) -> Text {
    let picked = value.to_string().into_bytes();
    Text {
        bytes: picked.len() as u64,
        picked,
        native_value: None,
    }
}

fn related(
    lines: &mut Lines<'_>,
    identity: &Identity<'_>,
    at: u64,
    complete: u64,
    remaining: &mut u64,
    side: Side,
) -> Option<Vec<u8>> {
    let cap = (*remaining).min(NEARBY_BYTES);
    let mut budget = cap;
    let result = scan(lines, identity, at, complete, &mut budget, side);
    *remaining -= cap - budget;
    result
}

fn scan(
    lines: &mut Lines<'_>,
    identity: &Identity<'_>,
    mut at: u64,
    complete: u64,
    budget: &mut u64,
    side: Side,
) -> Option<Vec<u8>> {
    let mut raw = Vec::new();
    for _ in 0..NEARBY_RECORDS {
        let (start, end) = match side {
            Side::Before => (lines.previous(at.checked_sub(1)?, budget).ok()?, at),
            Side::After => (at, lines.next(at, complete, budget).ok()?),
        };
        if start >= end {
            return None;
        }
        raw.clear();
        lines.input(start, end).read_to_end(&mut raw).ok()?;
        let frame: Frame<Marker> = serde_json::from_slice(&raw).ok()?;
        let marker = frame.payload;
        let nested_turn = marker
            .internal_chat_message_metadata_passthrough
            .as_ref()
            .map(|metadata| metadata.turn_id.as_str());
        let direct_turn = marker.turn_id.as_deref();
        if [marker.thread_id.as_deref(), marker.session_id.as_deref()]
            .into_iter()
            .flatten()
            .any(|id| id != identity.thread)
            || [direct_turn, nested_turn]
                .into_iter()
                .flatten()
                .any(|id| id != identity.turn)
        {
            return None;
        }
        // Codex may insert usage records between the call, its native completion
        // and its output. Any other intervening record ends this local proof.
        if frame.kind == "token_usage_record"
            || (frame.kind == "event_msg" && marker.kind.as_deref() == Some("token_count"))
        {
            at = match side {
                Side::Before => start,
                Side::After => end,
            };
            continue;
        }
        let expected = match side {
            Side::Before => {
                marker.kind.as_deref() == Some("function_call")
                    && marker.name.as_deref() == Some("wait_agent")
                    && marker.namespace.as_deref() == Some("collaboration")
            }
            Side::After => marker.kind.as_deref() == Some("function_call_output"),
        };
        return (frame.kind == "response_item"
            && expected
            && marker.call_id.as_deref() == Some(identity.call)
            && direct_turn.or(nested_turn) == Some(identity.turn))
        .then_some(raw);
    }
    None
}
