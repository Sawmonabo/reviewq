//! The latest typed terminal failure of a Codex lead's rollout, which recovery resumes.

use std::io::Read;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::NativeTerminalFailure;
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use super::records::{ID_BYTES, invalid, object, string};
use super::{file, locator, reader::NameBudget};

/// Inspect the last lifecycle boundary in the verified Codex transcript. A
/// more recent start, normal completion or abort supersedes an older failure.
pub(super) fn latest_failure(
    source: &locator::Source,
    high_water: u64,
    cancelled: &AtomicBool,
) -> Option<NativeTerminalFailure> {
    let mut remaining = 8 * 1024 * 1024;
    let mut lines = file::Lines::new(&source.file, cancelled);
    let mut end = high_water;
    for _ in 0..256 {
        if end == 0 {
            break;
        }
        let start = lines.previous(end - 1, &mut remaining).ok()?;
        let item = event(lines.input(start, end)).ok()?;
        end = start;
        if item
            .thread
            .as_deref()
            .is_some_and(|thread| thread != source.native_id.as_str())
        {
            continue;
        }
        match item.kind.as_deref() {
            Some("task_complete" | "turn_complete") => {
                return item.typed_error.then_some(NativeTerminalFailure {
                    turn: item.turn?.parse().ok()?,
                    completed_at_ms: item.completed_at_ms?,
                });
            }
            Some("task_started" | "turn_started" | "turn_aborted") => return None,
            _ => {}
        }
    }
    None
}

#[derive(Default)]
struct Event {
    kind: Option<String>,
    thread: Option<String>,
    turn: Option<String>,
    typed_error: bool,
    completed_at_ms: Option<i64>,
}

/// Reads one rollout line. Only an `event_msg` keeps its payload type.
fn event(input: impl Read) -> Result<Event, AppError> {
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let mut outer = None;
    let mut result = Event::default();
    object(&mut reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| outer = value),
        "payload" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| match name {
                "type" => string(reader, ID_BYTES).map(|value| result.kind = value),
                "thread_id" => string(reader, ID_BYTES).map(|value| result.thread = value),
                "turn_id" => string(reader, ID_BYTES).map(|value| result.turn = value),
                "error" => {
                    result.typed_error = reader.peek().map_err(invalid)? == ValueType::Object;
                    reader.skip_value().map_err(invalid)
                }
                "completed_at" if reader.peek().map_err(invalid)? == ValueType::Number => {
                    result.completed_at_ms = reader
                        .next_number_as_string()
                        .map_err(invalid)?
                        .parse::<i64>()
                        .ok()
                        .filter(|time| *time > 0)
                        .and_then(|time| time.checked_mul(1000));
                    Ok(())
                }
                _ => reader.skip_value().map_err(invalid),
            })
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    if outer.as_deref() != Some("event_msg") {
        result.kind = None;
    }
    Ok(result)
}
