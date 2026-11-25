use std::io::Read;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::{HistorySession, VisibleKind};
use reviewq_domain::identity::NativeInputId;
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use serde_json::Value;

use super::locator::Source;
use super::{file, locator, records};

const SCAN_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROW: u64 = 8 * 1024 * 1024;
/// The name Claude records for a call of ReviewQ's publication action tool.
const ACTION_TOOL: &str = "mcp__reviewq__review_action";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    session_id: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    is_sidechain: bool,
    message: Option<Value>,
}

impl Row {
    fn blocks(&self) -> impl Iterator<Item = &Value> {
        self.message
            .as_ref()
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    }
}

pub(crate) struct ClaudeOpening {
    pub generation: String,
    pub position: u64,
    pub request: Option<NativeInputId>,
}

pub(crate) fn claude_workflow_opening(
    session: &HistorySession,
    call_id: &str,
    cancelled: &AtomicBool,
) -> Result<ClaudeOpening, AppError> {
    let source = source(session, cancelled)?;
    let mut budget = SCAN_BYTES;
    let mut end = complete_end(&source, &mut budget, cancelled)?;
    let mut opening = None;
    while end > 0 {
        let start = file::end_of_line(&source.file, end - 1, &mut budget, cancelled)?;
        if let Some(row) = row(&source, start, end, cancelled)? {
            if row.kind == "user"
                && row
                    .blocks()
                    .any(|block| block["type"] == "tool_result" && block["tool_use_id"] == call_id)
            {
                return Err(AppError::Conflict);
            }
            if row.kind == "assistant" {
                for block in row
                    .blocks()
                    .filter(|block| block["type"] == "tool_use" && block["id"] == call_id)
                {
                    if block["name"] != ACTION_TOOL || opening.replace(start).is_some() {
                        return Err(AppError::Conflict);
                    }
                }
            } else if let Some(request) = user_entry(&source, start, end, cancelled) {
                let position = opening.ok_or(AppError::NotFound)?;
                return Ok(ClaudeOpening {
                    generation: file::generation(&source)?,
                    position,
                    request: Some(request),
                });
            }
        }
        end = start;
    }
    let position = opening.ok_or(AppError::NotFound)?;
    Ok(ClaudeOpening {
        generation: file::generation(&source)?,
        position,
        request: None,
    })
}

fn source(session: &HistorySession, cancelled: &AtomicBool) -> Result<Source, AppError> {
    if session.binding.identity().provider() != HostProvider::Claude || session.parent.is_some() {
        return Err(AppError::Forbidden);
    }
    locator::locate(session, cancelled)?.ok_or(AppError::NotFound)
}

fn complete_end(
    source: &Source,
    budget: &mut u64,
    cancelled: &AtomicBool,
) -> Result<u64, AppError> {
    let length = source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len();
    file::end_of_line(&source.file, length, budget, cancelled)
}

fn row(
    source: &Source,
    start: u64,
    end: u64,
    cancelled: &AtomicBool,
) -> Result<Option<Row>, AppError> {
    let length = end.checked_sub(start).ok_or(AppError::Internal)?;
    if length > MAX_ROW {
        return Err(AppError::PayloadTooLarge);
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file::input(&source.file, start, end, cancelled)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::Unavailable)?;
    let row: Row = serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
    Ok(
        (!row.is_sidechain && row.session_id.as_deref() == Some(source.native_id.as_str()))
            .then_some(row),
    )
}

fn user_entry(
    source: &Source,
    start: u64,
    end: u64,
    cancelled: &AtomicBool,
) -> Option<NativeInputId> {
    let row = records::project(
        HostProvider::Claude,
        file::input(&source.file, start, end, cancelled),
        &source.native_id,
        None,
        0,
        0,
    )?;
    (row.kind == VisibleKind::User).then_some(row.id)
}
