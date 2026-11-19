use std::io::Read;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::{HistorySession, VisibleKind};
use reviewq_app::workflow::{ExpectedAcknowledgement, NativeCall, WorkflowTool};
use reviewq_domain::identity::{NativeInputId, ReportHash};
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::locator::Source;
use super::{file, locator, records};

const SCAN_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROW: u64 = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    uuid: Option<String>,
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
    tool: WorkflowTool,
    cancelled: &AtomicBool,
) -> Result<ClaudeOpening, AppError> {
    let source = source(session, cancelled)?;
    let mut budget = SCAN_BYTES;
    let mut end = complete_end(&source, &mut budget, cancelled)?;
    let name = match tool {
        WorkflowTool::Report => "mcp__reviewq__report_handoff",
        WorkflowTool::Action => "mcp__reviewq__review_action",
    };
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
            if row.kind == "assistant"
                && row.blocks().any(|block| {
                    block["type"] == "tool_use" && block["id"] == call_id && block["name"] == name
                })
            {
                if opening.replace(start).is_some() {
                    return Err(AppError::Conflict);
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

pub(crate) struct ClaudeReportResult {
    pub entry: NativeInputId,
    pub response: Option<Vec<u8>>,
}

pub(crate) fn claude_workflow_result(
    session: &HistorySession,
    call: &NativeCall,
    expected: &ExpectedAcknowledgement,
    final_response: Option<(NativeInputId, ReportHash)>,
    cancelled: &AtomicBool,
) -> Result<Option<ClaudeReportResult>, AppError> {
    let source = source(session, cancelled)?;
    let mut budget = SCAN_BYTES;
    let complete = complete_end(&source, &mut budget, cancelled)?;
    let mut offset = call.source_position;
    if file::generation(&source)? != call.source_generation
        || offset >= complete
        || !file::boundary(&source.file, offset)?
    {
        return Err(AppError::Conflict);
    }
    let opening_end = file::next_line(&source.file, offset, complete, &mut budget, cancelled)?;
    let open = row(&source, offset, opening_end, cancelled)?.ok_or(AppError::Conflict)?;
    if open.kind != "assistant"
        || !open.blocks().any(|block| {
            block["type"] == "tool_use"
                && block["id"] == call.call_id
                && block["name"] == "mcp__reviewq__report_handoff"
        })
    {
        return Err(AppError::Conflict);
    }

    let mut result = None;
    offset = opening_end;
    while offset < complete {
        let end = file::next_line(&source.file, offset, complete, &mut budget, cancelled)?;
        if let Some(row) = row(&source, offset, end, cancelled)? {
            if row.kind == "user" {
                for block in row
                    .blocks()
                    .filter(|b| b["type"] == "tool_result" && b["tool_use_id"] == call.call_id)
                {
                    if result.is_some()
                        || block["is_error"].as_bool() == Some(true)
                        || !acknowledgement(&block["content"], expected)
                    {
                        return Err(AppError::Conflict);
                    }
                    result = Some(ClaudeReportResult {
                        entry: row
                            .uuid
                            .as_deref()
                            .ok_or(AppError::Conflict)?
                            .parse()
                            .map_err(|_| AppError::Conflict)?,
                        response: None,
                    });
                }
                if user_entry(&source, offset, end, cancelled).is_some() {
                    break;
                }
            }
            if let (Some(saved), Some((id, hash))) = (&mut result, &final_response)
                && row.kind == "assistant"
                && row.uuid.as_deref() == Some(id.as_str())
            {
                let text = row
                    .blocks()
                    .filter_map(|block| {
                        (block["type"] == "text")
                            .then(|| block["text"].as_str())
                            .flatten()
                    })
                    .collect::<String>()
                    .into_bytes();
                let digest = ReportHash::from_sha256(Sha256::digest(&text).into());
                if !text.is_empty() && digest == *hash {
                    saved.response = Some(text);
                }
                break;
            }
            if result.is_some() && final_response.is_none() {
                break;
            }
        }
        offset = end;
    }
    Ok(result)
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

fn acknowledgement(content: &Value, expected: &ExpectedAcknowledgement) -> bool {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Ack {
        receipt_id: String,
        state: String,
        declaration_sha256: String,
    }
    let text = match content {
        Value::String(text) => text.as_str(),
        Value::Array(items) if items.len() == 1 && items[0]["type"] == "text" => {
            let Some(text) = items[0]["text"].as_str() else {
                return false;
            };
            text
        }
        _ => return false,
    };
    text.len() <= 1_024
        && serde_json::from_str::<Ack>(text).is_ok_and(|value| {
            value.receipt_id == expected.receipt_id.as_str()
                && value.declaration_sha256 == expected.declaration_sha256.as_str()
                && matches!(value.state.as_str(), "pending_completion" | "recorded")
        })
}
