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
use super::report::{acknowledgement, native_arguments_hash};
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
    pub ordinal: u32,
    pub request: Option<NativeInputId>,
}

pub(crate) fn claude_workflow_opening(
    session: &HistorySession,
    call_id: &str,
    tool: WorkflowTool,
    arguments: Option<&Value>,
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
            if row.kind == "assistant" {
                for (index, block) in row
                    .blocks()
                    .enumerate()
                    .filter(|(_, block)| block["type"] == "tool_use" && block["id"] == call_id)
                {
                    if block["name"] != name
                        || arguments.is_some_and(|args| block.get("input") != Some(args))
                    {
                        return Err(AppError::Conflict);
                    }
                    let ordinal = if arguments.is_some() {
                        u32::try_from(index + 1).map_err(|_| AppError::Unavailable)?
                    } else {
                        0
                    };
                    if opening.replace((start, ordinal)).is_some() {
                        return Err(AppError::Conflict);
                    }
                }
            } else if let Some(request) = user_entry(&source, start, end, cancelled) {
                let (position, ordinal) = opening.ok_or(AppError::NotFound)?;
                return Ok(ClaudeOpening {
                    generation: file::generation(&source)?,
                    position,
                    ordinal,
                    request: Some(request),
                });
            }
        }
        end = start;
    }
    let (position, ordinal) = opening.ok_or(AppError::NotFound)?;
    Ok(ClaudeOpening {
        generation: file::generation(&source)?,
        position,
        ordinal,
        request: None,
    })
}

/// A Mod can receive tool.call before Claude flushes the assistant tool_use row.
pub(crate) fn claude_workflow_prompt(
    session: &HistorySession,
    expected: &NativeInputId,
    cancelled: &AtomicBool,
) -> Result<(String, u64), AppError> {
    let source = source(session, cancelled)?;
    let mut budget = SCAN_BYTES;
    let mut end = complete_end(&source, &mut budget, cancelled)?;
    let mut lines = file::Lines::new(&source.file, cancelled);
    while end > 0 {
        let start = lines.previous(end - 1, &mut budget)?;
        if let Some(id) = user_entry(&source, start, end, cancelled) {
            if &id != expected {
                return Err(AppError::Conflict);
            }
            return Ok((file::generation(&source)?, start));
        }
        end = start;
    }
    Err(AppError::NotFound)
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
    let mut opened = match open.kind.as_str() {
        "assistant"
            if open.blocks().enumerate().any(|(index, block)| {
                (call.source_ordinal == 0 || index + 1 == call.source_ordinal as usize)
                    && report_block(block, call)
            }) =>
        {
            true
        }
        "user"
            if call.arguments_sha256.is_some()
                && user_entry(&source, offset, opening_end, cancelled)
                    .is_some_and(|id| Some(&id) == call.request_entry_id.as_ref()) =>
        {
            false
        }
        _ => return Err(AppError::Conflict),
    };

    let mut result = None;
    offset = opening_end;
    while offset < complete {
        let end = file::next_line(&source.file, offset, complete, &mut budget, cancelled)?;
        if let Some(row) = row(&source, offset, end, cancelled)? {
            if row.kind == "assistant" && call.arguments_sha256.is_some() {
                for block in row
                    .blocks()
                    .filter(|block| block["type"] == "tool_use" && block["id"] == call.call_id)
                {
                    if opened || !report_block(block, call) {
                        return Err(AppError::Conflict);
                    }
                    opened = true;
                }
            }
            if row.kind == "user" {
                for block in row
                    .blocks()
                    .filter(|b| b["type"] == "tool_result" && b["tool_use_id"] == call.call_id)
                {
                    if !opened
                        || result.is_some()
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
                let blocks: Vec<_> = row
                    .blocks()
                    .filter_map(|block| {
                        (block["type"] == "text")
                            .then(|| block["text"].as_str())
                            .flatten()
                    })
                    .collect();
                let text = blocks.concat();
                let matches = |value: &str| {
                    !value.is_empty()
                        && ReportHash::from_sha256(Sha256::digest(value.as_bytes()).into()) == *hash
                };
                // Earlier Mods hash Claude's turn.complete projection: text
                // blocks joined by LF, then ECMAScript String.prototype.trim().
                if matches(&text)
                    || (row.message.as_ref().is_some_and(|m| {
                        m["role"] == "assistant" && m["stop_reason"] == "end_turn"
                    }) && matches(blocks.join("\n").trim_matches(js_whitespace)))
                {
                    saved.response = Some(text.into_bytes());
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

fn report_block(block: &Value, call: &NativeCall) -> bool {
    block["type"] == "tool_use"
        && block["id"] == call.call_id
        && block["name"] == "mcp__reviewq__report_handoff"
        && call.arguments_sha256.as_ref().is_none_or(|hash| {
            block
                .get("input")
                .is_some_and(|input| native_arguments_hash(input.clone()).as_ref() == Ok(hash))
        })
}

fn js_whitespace(c: char) -> bool {
    c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace())
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

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::Write;

    use reviewq_domain::session::{
        AbsolutePath, LeadTarget, NativeBinding, NativeIdentity, SessionTarget,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn buffered_claude_call_keeps_its_prompt_anchor_and_rechecks_the_original_arguments() {
        let profile = tempfile::tempdir().unwrap();
        let directory = profile.path().join("projects").join("project");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("thread.jsonl");
        let mut file = File::create(&path).unwrap();
        let root = AbsolutePath::from_canonical(profile.path().to_path_buf()).unwrap();
        let session = HistorySession {
            target: SessionTarget::new(
                "review".parse().unwrap(),
                "run".parse().unwrap(),
                "session".parse().unwrap(),
            ),
            binding: NativeBinding::new(
                NativeIdentity::new(
                    HostProvider::Claude,
                    root.clone(),
                    "thread".parse().unwrap(),
                ),
                root,
            ),
            source: Some(AbsolutePath::from_canonical(path).unwrap()),
            parent: None,
        };
        let prompt = "prompt".parse().unwrap();
        let arguments = json!({"handoff":{"outcome":"needs_input","reason":"fixture"}});
        let cancelled = AtomicBool::new(false);
        let row = json!({"type":"user","uuid":"prompt","cwd":profile.path(),"sessionId":"thread","message":{"role":"user","content":"Review the project"}});
        writeln!(file, "{row}").unwrap();
        let anchor = claude_workflow_prompt(&session, &prompt, &cancelled).unwrap();
        let call = NativeCall {
            lead: LeadTarget::new(session.target.clone(), session.binding.clone()),
            turn_id: "turn".parse().unwrap(),
            root_turn_id: None,
            call_id: "toolu_fixture".into(),
            source_generation: anchor.0.clone(),
            source_position: anchor.1,
            source_ordinal: 2,
            request_entry_id: Some(prompt.clone()),
            arguments_sha256: Some(native_arguments_hash(arguments.clone()).unwrap()),
            tool: WorkflowTool::Report,
        };
        let ack = ExpectedAcknowledgement {
            receipt_id: "receipt".parse().unwrap(),
            declaration_sha256: "f".repeat(64).parse().unwrap(),
        };
        let result =
            |call: &NativeCall| claude_workflow_result(&session, call, &ack, None, &cancelled);
        assert!(result(&call).unwrap().is_none());
        let row = json!({"type":"assistant","uuid":"assistant","sessionId":"thread","message":{"role":"assistant","content":[
            {"type":"tool_use","name":"mcp__reviewq__report_handoff","id":"toolu_fixture","input":arguments}
        ]}});
        writeln!(file, "{row}").unwrap();
        let direct = claude_workflow_opening(
            &session,
            &call.call_id,
            WorkflowTool::Report,
            Some(&arguments),
            &cancelled,
        )
        .unwrap();
        let receipt = json!({"receiptId":ack.receipt_id.as_str(),"declarationSha256":ack.declaration_sha256.as_str(),"state":"pending_completion"}).to_string();
        let row = json!({"type":"user","uuid":"result","sessionId":"thread","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_fixture","content":[{"type":"text","text":receipt}]}
        ]}});
        writeln!(file, "{row}").unwrap();
        assert_eq!(
            claude_workflow_prompt(&session, &prompt, &cancelled).unwrap(),
            anchor
        );
        let original = "\u{feff}\n# Report\nAuthored  spacing.  \r\n\n";
        let row = json!({"type":"assistant","uuid":"final","sessionId":"thread","message":{"role":"assistant","stop_reason":"end_turn","content":[
            {"type":"text","text":"\u{feff}\n# Report\n"},{"type":"text","text":"Authored  spacing.  \r\n\n"}
        ]}});
        writeln!(file, "{row}").unwrap();
        let response = |entry: &str, observed: &str| {
            let hash = ReportHash::from_sha256(Sha256::digest(observed.as_bytes()).into());
            claude_workflow_result(
                &session,
                &call,
                &ack,
                Some((entry.parse().unwrap(), hash)),
                &cancelled,
            )
            .unwrap()
            .unwrap()
            .response
        };
        assert_eq!(
            response("final", "# Report\n\nAuthored  spacing.").as_deref(),
            Some(original.as_bytes())
        );
        assert_eq!(
            response("final", original).as_deref(),
            Some(original.as_bytes())
        );
        assert_eq!(response("final", "# Report\n\nAuthored spacing."), None);
        assert_eq!(response("different-entry", original), None);
        let directly_verified = NativeCall {
            source_position: direct.position,
            source_ordinal: direct.ordinal,
            ..call.clone()
        };
        for mut native in [call, directly_verified] {
            assert_eq!(result(&native).unwrap().unwrap().entry.as_str(), "result");
            native.arguments_sha256 = Some(native_arguments_hash(json!({"handoff":{}})).unwrap());
            assert_eq!(result(&native).err(), Some(AppError::Conflict));
        }
    }
}
