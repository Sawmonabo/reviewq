//! Codex settings and turn-creation evidence from its verified rollout.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_domain::session::{HostProvider, NativeBinding};
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use super::reader::NameBudget;
use super::records::{ID_BYTES, invalid, object, string};
use super::{Cancel, file, locator};

const SEARCH_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) async fn codex_turn_settings(
    binding: NativeBinding,
    path: PathBuf,
    turn: String,
) -> Result<Option<(String, String)>, AppError> {
    let thread = binding.identity().native_id().to_string();
    scan(binding, path, move |row| {
        let applied = row.outer.as_deref() == Some("event_msg")
            && row.kind.as_deref() == Some("thread_settings_applied")
            && row.thread.as_ref() == Some(&thread);
        let context =
            row.outer.as_deref() == Some("turn_context") && row.turn.as_ref() == Some(&turn);
        (applied || context).then(|| row.model.zip(row.effort))
    })
    .await
    .map(Option::flatten)
}

pub(crate) async fn codex_exact_turn_settings(
    binding: NativeBinding,
    path: PathBuf,
    turn: String,
) -> Result<Option<(String, String)>, AppError> {
    scan(binding, path, move |row| {
        let exact =
            row.outer.as_deref() == Some("turn_context") && row.turn.as_ref() == Some(&turn);
        exact.then(|| row.model.zip(row.effort))
    })
    .await
    .map(Option::flatten)
}

pub(crate) async fn codex_turn_started(
    binding: NativeBinding,
    path: PathBuf,
    turn: String,
    marker: String,
) -> Result<Option<bool>, AppError> {
    scan(binding, path, move |row| {
        let exact = row.outer.as_deref() == Some("event_msg")
            && matches!(row.kind.as_deref(), Some("task_started" | "turn_started"))
            && row.turn.as_ref() == Some(&turn);
        exact.then(|| {
            row.attributed_turn.as_ref() == Some(&turn) && row.trigger.as_ref() == Some(&marker)
        })
    })
    .await
}

/// Resolves the native turn boundary immediately preceding an opening item.
pub(super) fn turn_before(
    source: &locator::Source,
    mut end: u64,
    cancelled: &AtomicBool,
) -> Result<Option<String>, AppError> {
    let mut budget = file::READ_BUDGET;
    let mut lines = file::Lines::new(&source.file, cancelled);
    for _ in 0..file::MAX_RECORDS {
        if end == 0 || budget == 0 {
            break;
        }
        let start = lines.previous(end - 1, &mut budget)?;
        let row = read(lines.input(start, end))?;
        if row.turn.is_some()
            && (row.outer.as_deref() == Some("turn_context")
                || (row.outer.as_deref() == Some("event_msg")
                    && matches!(row.kind.as_deref(), Some("task_started" | "turn_started"))))
        {
            return Ok(row.turn);
        }
        end = start;
    }
    Ok(None)
}

async fn scan<T: Send + 'static>(
    binding: NativeBinding,
    path: PathBuf,
    mut select: impl FnMut(Metadata) -> Option<T> + Send + 'static,
) -> Result<Option<T>, AppError> {
    if binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::Forbidden);
    }
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let stopped = cancel.0.clone();
    tokio::task::spawn_blocking(move || {
        let source =
            locator::verify(&binding, None, &path, &stopped)?.ok_or(AppError::Unavailable)?;
        let length = source
            .file
            .metadata()
            .map_err(|_| AppError::Unavailable)?
            .len();
        let mut budget = SEARCH_BYTES;
        let mut end = file::end_of_line(&source.file, length, &mut budget, &stopped)?;
        while end > 0 {
            let start = file::end_of_line(&source.file, end - 1, &mut budget, &stopped)?;
            if let Ok(row) = read(file::input(&source.file, start, end, &stopped))
                && let Some(value) = select(row)
            {
                return Ok(Some(value));
            }
            end = start;
        }
        Ok(None)
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

#[derive(Default)]
struct Metadata {
    outer: Option<String>,
    kind: Option<String>,
    thread: Option<String>,
    turn: Option<String>,
    attributed_turn: Option<String>,
    trigger: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

fn read(input: impl Read) -> Result<Metadata, AppError> {
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let mut row = Metadata::default();
    object(&mut reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| row.outer = value),
        "payload" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| match name {
                "type" => string(reader, ID_BYTES).map(|value| row.kind = value),
                "thread_id" => string(reader, ID_BYTES).map(|value| row.thread = value),
                "turn_id" => string(reader, ID_BYTES).map(|value| row.turn = value),
                "model" => string(reader, 128).map(|value| row.model = value),
                "effort" => string(reader, 64).map(|value| row.effort = value),
                "thread_settings" if reader.peek().map_err(invalid)? == ValueType::Object => {
                    object(reader, |name, reader| match name {
                        "model" => string(reader, 128).map(|value| row.model = value),
                        "reasoning_effort" => string(reader, 64).map(|value| row.effort = value),
                        _ => reader.skip_value().map_err(invalid),
                    })
                }
                "turn_attribution" if reader.peek().map_err(invalid)? == ValueType::Object => {
                    object(reader, |name, reader| match name {
                        "turn_id" => {
                            string(reader, ID_BYTES).map(|value| row.attributed_turn = value)
                        }
                        "turn_trigger" => string(reader, ID_BYTES).map(|value| row.trigger = value),
                        _ => reader.skip_value().map_err(invalid),
                    })
                }
                _ => reader.skip_value().map_err(invalid),
            })
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    Ok(row)
}
