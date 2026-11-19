use std::io::Read;

use reviewq_app::access::AppError;
use reviewq_app::history::{ToolState, VisibleKind, VisiblePartKind};
use reviewq_domain::identity::{NativeInputId, NativeSessionId, NativeTurnId};
use reviewq_domain::session::HostProvider;
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use super::reader::NameBudget;
use super::tools::{self, MAX_NATIVE_PARTS};

pub(super) const ID_BYTES: u64 = 512;

pub(super) struct Record {
    pub id: NativeInputId,
    pub turn: Option<NativeTurnId>,
    pub at_ms: Option<i64>,
    pub kind: VisibleKind,
    pub text: Vec<u8>,
    pub bytes: u64,
    pub parts: Vec<Part>,
}

pub(super) struct Part {
    pub index: u16,
    pub kind: VisiblePartKind,
    pub body: Text,
}

#[derive(Clone, Copy)]
pub(super) struct Window {
    pub start: u64,
    pub limit: usize,
    pub part: Option<u16>,
}

#[derive(Default)]
pub(super) struct Text {
    pub bytes: u64,
    pub picked: Vec<u8>,
}

#[derive(Default)]
struct Message {
    id: Option<String>,
    kind: Option<String>,
    text: Text,
    parts: Vec<Part>,
    argument: Text,
    result: Text,
    tool: Option<String>,
    status: Option<String>,
}

#[derive(Default)]
struct Event {
    kind: Option<String>,
    thread: Option<String>,
    turn: Option<String>,
    item: Message,
}

pub(super) fn project(
    provider: HostProvider,
    input: impl Read,
    native: &NativeSessionId,
    root_session: Option<&NativeSessionId>,
    offset: u64,
    limit: usize,
) -> Option<Record> {
    project_window(
        provider,
        input,
        native,
        root_session,
        Window {
            start: offset,
            limit,
            part: None,
        },
    )
}

pub(super) fn project_window(
    provider: HostProvider,
    input: impl Read,
    native: &NativeSessionId,
    root_session: Option<&NativeSessionId>,
    window: Window,
) -> Option<Record> {
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let result = match provider {
        HostProvider::Codex => codex(&mut reader, native, window),
        HostProvider::Claude => claude(&mut reader, native, root_session, window),
    };
    let (mut item, kind, at_ms, turn) = result.ok()??;
    reader.consume_trailing_whitespace().ok()?;
    let id = item.id?.parse().ok()?;
    if item.text.bytes == 0 && item.parts.is_empty() {
        return None;
    }
    if item
        .parts
        .iter()
        .all(|part| matches!(part.kind, VisiblePartKind::Text(_)))
    {
        item.parts.clear();
    } else {
        for part in &mut item.parts {
            if matches!(part.kind, VisiblePartKind::Text(_)) {
                part.kind = VisiblePartKind::Text(kind);
            }
        }
    }
    Some(Record {
        id,
        turn: turn.and_then(|value| value.parse().ok()),
        at_ms,
        kind,
        text: item.text.picked,
        bytes: item.text.bytes,
        parts: item.parts,
    })
}

type Projection = Option<(Message, VisibleKind, Option<i64>, Option<String>)>;

fn codex<R: Read>(
    reader: &mut JsonStreamReader<R>,
    native: &NativeSessionId,
    window: Window,
) -> Result<Projection, AppError> {
    let mut event = Event::default();
    let mut record_type = None;
    let mut stamp = None;
    object(reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| record_type = value),
        "timestamp" => string(reader, ID_BYTES).map(|value| stamp = date(value)),
        "payload" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| match name {
                "type" => string(reader, ID_BYTES).map(|value| event.kind = value),
                "thread_id" => string(reader, ID_BYTES).map(|value| event.thread = value),
                "turn_id" => string(reader, ID_BYTES).map(|value| event.turn = value),
                "item" if reader.peek().map_err(invalid)? == ValueType::Object => {
                    message(reader, window).map(|value| event.item = value)
                }
                _ => reader.skip_value().map_err(invalid),
            })
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    if record_type.as_deref() != Some("event_msg")
        || event.kind.as_deref() != Some("item_completed")
        || event.thread.as_deref() != Some(native.as_str())
        || event.turn.as_ref().is_none_or(String::is_empty)
    {
        return Ok(None);
    }
    let kind = match event.item.kind.as_deref() {
        Some("UserMessage") => VisibleKind::User,
        Some("AgentMessage") => VisibleKind::Assistant,
        Some("Plan" | "plan") => {
            event.item.parts.push(Part {
                index: 0,
                kind: VisiblePartKind::Plan,
                body: std::mem::take(&mut event.item.text),
            });
            VisibleKind::Assistant
        }
        Some(
            "CommandExecution"
            | "commandExecution"
            | "FileChange"
            | "fileChange"
            | "McpToolCall"
            | "mcpToolCall"
            | "DynamicToolCall"
            | "dynamicToolCall"
            | "WebSearch"
            | "webSearch"
            | "ImageView"
            | "imageView"
            | "CollabAgentToolCall"
            | "collabAgentToolCall",
        ) => {
            let Some(call): Option<NativeInputId> =
                event.item.id.as_deref().and_then(|id| id.parse().ok())
            else {
                return Ok(None);
            };
            let name = event
                .item
                .tool
                .take()
                .or_else(|| event.item.kind.clone())
                .unwrap_or_default();
            if !display_id(&name) {
                return Ok(None);
            }
            let state = tools::state(event.item.status.as_deref());
            event.item.parts.push(Part {
                index: 0,
                kind: VisiblePartKind::ToolCall {
                    call: call.clone(),
                    label: tools::label(&name),
                    name,
                    state,
                },
                body: std::mem::take(&mut event.item.argument),
            });
            if event.item.result.bytes > 0 {
                event.item.parts.push(Part {
                    index: 1,
                    kind: VisiblePartKind::ToolResult { call, state },
                    body: std::mem::take(&mut event.item.result),
                });
            }
            VisibleKind::Assistant
        }
        _ => return Ok(None),
    };
    Ok(Some((event.item, kind, stamp, event.turn)))
}

fn claude<R: Read>(
    reader: &mut JsonStreamReader<R>,
    native: &NativeSessionId,
    root: Option<&NativeSessionId>,
    window: Window,
) -> Result<Projection, AppError> {
    let mut item = Message::default();
    let mut id = None;
    let mut role = None;
    let mut stamp = None;
    let mut hidden = false;
    let mut sidechain = false;
    let mut session = None;
    let mut agent = None;
    let mut secret_answer = false;
    object(reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| role = value),
        "uuid" => string(reader, ID_BYTES).map(|value| id = value),
        "timestamp" => string(reader, ID_BYTES).map(|value| stamp = date(value)),
        "sessionId" => string(reader, ID_BYTES).map(|value| session = value),
        "agentId" => string(reader, ID_BYTES).map(|value| agent = value),
        "isSidechain" => reader
            .next_bool()
            .map(|value| sidechain = value)
            .map_err(invalid),
        "isMeta" | "isCompactSummary" | "isVisibleInTranscriptOnly"
            if reader.peek().map_err(invalid)? == ValueType::Boolean =>
        {
            reader
                .next_bool()
                .map(|value| hidden |= value)
                .map_err(invalid)
        }
        "toolUseResult" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| {
                secret_answer |= matches!(name, "answers" | "answer" | "secret");
                reader.skip_value().map_err(invalid)
            })
        }
        "message" if reader.peek().map_err(invalid)? == ValueType::Object => {
            message(reader, window).map(|value| item = value)
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    let matched = match root {
        Some(root) => {
            sidechain
                && session.as_deref() == Some(root.as_str())
                && agent.as_deref() == Some(native.as_str())
        }
        None => !sidechain && session.as_deref() == Some(native.as_str()),
    };
    if hidden || !matched {
        return Ok(None);
    }
    if secret_answer {
        item.parts
            .retain(|part| !matches!(part.kind, VisiblePartKind::ToolResult { .. }));
    }
    let kind = match (role.as_deref(), item.kind.as_deref()) {
        (Some("user"), Some("user")) if item.text.bytes > 0 => VisibleKind::User,
        (Some("user"), Some("user")) => VisibleKind::Assistant,
        (Some("assistant"), Some("assistant")) => VisibleKind::Assistant,
        _ => return Ok(None),
    };
    item.id = id;
    Ok(Some((item, kind, stamp, None)))
}

fn message<R: Read>(reader: &mut JsonStreamReader<R>, window: Window) -> Result<Message, AppError> {
    let mut item = Message::default();
    object(reader, |name, reader| match name {
        "type" | "role" => string(reader, ID_BYTES).map(|value| item.kind = value),
        "id" => string(reader, ID_BYTES).map(|value| item.id = value),
        "content" => contents(reader, window, &mut item),
        "text" => tools::body(reader, window).map(|value| item.text = value),
        "tool" | "name" => string(reader, 128).map(|value| item.tool = value),
        "status" => string(reader, 32).map(|value| item.status = value),
        "command" | "arguments" | "query" | "changes" | "prompt" => {
            tools::body(reader, tools::part_window(window, 0)).map(|value| item.argument = value)
        }
        "aggregated_output" | "aggregatedOutput" | "result" | "output" => {
            tools::body(reader, tools::part_window(window, 1)).map(|value| item.result = value)
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    Ok(item)
}

fn contents<R: Read>(
    reader: &mut JsonStreamReader<R>,
    window: Window,
    item: &mut Message,
) -> Result<(), AppError> {
    match reader.peek().map_err(invalid)? {
        ValueType::String => {
            item.text = capture(reader, window, 0)?;
            Ok(())
        }
        ValueType::Array => {
            reader.begin_array().map_err(invalid)?;
            let mut index = 0_u16;
            while reader.has_next().map_err(invalid)? {
                block(reader, window, index, item)?;
                index = index.saturating_add(1);
            }
            reader.end_array().map_err(invalid)
        }
        _ => reader.skip_value().map_err(invalid),
    }
}

fn block<R: Read>(
    reader: &mut JsonStreamReader<R>,
    window: Window,
    index: u16,
    item: &mut Message,
) -> Result<(), AppError> {
    if reader.peek().map_err(invalid)? != ValueType::Object {
        return reader.skip_value().map_err(invalid);
    }
    let mut kind = None;
    let mut name = None;
    let mut call = None;
    let mut error = false;
    let mut plain = Text::default();
    let mut body = Text::default();
    let retained = index < MAX_NATIVE_PARTS;
    let preview_window = tools::part_window(window, index);
    object(reader, |field, reader| match field {
        "type" => string(reader, ID_BYTES).map(|value| kind = value),
        "name" if retained => string(reader, 128).map(|value| name = value),
        "id" | "tool_use_id" if retained => string(reader, 128).map(|value| call = value),
        "is_error" if reader.peek().map_err(invalid)? == ValueType::Boolean => {
            error = reader.next_bool().map_err(invalid)?;
            Ok(())
        }
        "text" | "output_text" if reader.peek().map_err(invalid)? == ValueType::String => capture(
            reader,
            if window.part.is_some() {
                preview_window
            } else {
                window
            },
            if window.part.is_some() {
                0
            } else {
                item.text.bytes
            },
        )
        .map(|value| plain = value),
        "input" | "content" if retained => {
            tools::body(reader, preview_window).map(|value| body = value)
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    let part = match kind.as_deref() {
        Some("Text" | "text" | "input_text" | "output_text") => {
            item.text.bytes = item
                .text
                .bytes
                .checked_add(plain.bytes)
                .ok_or(AppError::PayloadTooLarge)?;
            if window.part.is_none() {
                item.text.picked.extend_from_slice(&plain.picked);
            }
            plain.picked.truncate(preview_window.limit);
            Part {
                index,
                kind: VisiblePartKind::Text(VisibleKind::Assistant),
                body: plain,
            }
        }
        Some("tool_use") => {
            let Some(call) = call.and_then(|id| id.parse().ok()) else {
                return Ok(());
            };
            let Some(name) = name.filter(|value| display_id(value)) else {
                return Ok(());
            };
            let label = tools::label(&name);
            let kind = match label {
                "Plan" => VisiblePartKind::Plan,
                "AgentSpawn" => VisiblePartKind::Assignment,
                _ => VisiblePartKind::ToolCall {
                    call,
                    name,
                    label,
                    state: ToolState::Unknown,
                },
            };
            Part { index, kind, body }
        }
        Some("tool_result") => {
            let Some(call) = call.and_then(|id| id.parse().ok()) else {
                return Ok(());
            };
            Part {
                index,
                kind: VisiblePartKind::ToolResult {
                    call,
                    state: if error {
                        ToolState::Failed
                    } else {
                        ToolState::Completed
                    },
                },
                body,
            }
        }
        _ => return Ok(()),
    };
    if retained {
        item.parts.push(part);
    }
    Ok(())
}

fn display_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

pub(super) fn capture<R: Read>(
    reader: &mut JsonStreamReader<R>,
    window: Window,
    origin: u64,
) -> Result<Text, AppError> {
    let mut stream = reader.next_string_reader().map_err(invalid)?;
    let mut result = Text::default();
    let mut buffer = [0; 16 * 1024];
    let wanted_end = window
        .start
        .checked_add(window.limit as u64)
        .ok_or(AppError::InvalidRequest)?;
    loop {
        let size = stream.read(&mut buffer).map_err(invalid)?;
        if size == 0 {
            return Ok(result);
        }
        let first = origin
            .checked_add(result.bytes)
            .ok_or(AppError::PayloadTooLarge)?;
        let end = first
            .checked_add(size as u64)
            .ok_or(AppError::PayloadTooLarge)?;
        let low = window.start.max(first);
        let high = wanted_end.min(end);
        if low < high {
            result
                .picked
                .extend_from_slice(&buffer[(low - first) as usize..(high - first) as usize]);
        }
        result.bytes += size as u64;
    }
}

pub(super) fn string<R: Read>(
    reader: &mut JsonStreamReader<R>,
    limit: u64,
) -> Result<Option<String>, AppError> {
    if reader.peek().map_err(invalid)? != ValueType::String {
        reader.skip_value().map_err(invalid)?;
        return Ok(None);
    }
    let mut bytes = Vec::new();
    reader
        .next_string_reader()
        .map_err(invalid)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    if bytes.len() as u64 > limit {
        return Err(AppError::Unavailable);
    }
    String::from_utf8(bytes).map(Some).map_err(invalid)
}

pub(super) fn object<R: Read>(
    reader: &mut JsonStreamReader<R>,
    mut field: impl FnMut(&str, &mut JsonStreamReader<R>) -> Result<(), AppError>,
) -> Result<(), AppError> {
    reader.begin_object().map_err(invalid)?;
    let mut count = 0;
    while reader.has_next().map_err(invalid)? {
        let name = reader.next_name_owned().map_err(invalid)?;
        count += 1;
        if name.len() > 256 || count > 256 {
            return Err(AppError::Unavailable);
        }
        field(&name, reader)?;
    }
    reader.end_object().map_err(invalid)
}

fn date(raw: Option<String>) -> Option<i64> {
    raw?.parse::<jiff::Timestamp>()
        .ok()
        .map(|stamp| stamp.as_millisecond())
}

pub(super) fn invalid<T>(_: T) -> AppError {
    AppError::Unavailable
}
