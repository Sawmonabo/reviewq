use std::io::Read;

use reviewq_app::access::AppError;
use reviewq_app::history::VisibleKind;
use reviewq_domain::identity::{NativeInputId, NativeSessionId, NativeTurnId};
use reviewq_domain::session::HostProvider;
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use super::reader::NameBudget;

pub(super) const ID_BYTES: u64 = 512;

pub(super) struct Record {
    pub id: NativeInputId,
    pub turn: Option<NativeTurnId>,
    pub at_ms: Option<i64>,
    pub kind: VisibleKind,
    pub text: Vec<u8>,
    pub bytes: u64,
}

#[derive(Clone, Copy)]
struct Window {
    start: u64,
    limit: usize,
}

#[derive(Default)]
struct Text {
    bytes: u64,
    picked: Vec<u8>,
}

#[derive(Default)]
struct Message {
    id: Option<String>,
    kind: Option<String>,
    text: Text,
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
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let window = Window {
        start: offset,
        limit,
    };
    let result = match provider {
        HostProvider::Codex => codex(&mut reader, native, window),
        HostProvider::Claude => claude(&mut reader, native, root_session, window),
    };
    let (item, kind, at_ms, turn) = result.ok()??;
    reader.consume_trailing_whitespace().ok()?;
    let id = item.id?.parse().ok()?;
    if item.text.bytes == 0 {
        return None;
    }
    Some(Record {
        id,
        turn: turn.and_then(|value| value.parse().ok()),
        at_ms,
        kind,
        text: item.text.picked,
        bytes: item.text.bytes,
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
    let kind = match (role.as_deref(), item.kind.as_deref()) {
        (Some("user"), Some("user")) => VisibleKind::User,
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
        "content" => contents(reader, window).map(|value| item.text = value),
        _ => reader.skip_value().map_err(invalid),
    })?;
    Ok(item)
}

fn contents<R: Read>(reader: &mut JsonStreamReader<R>, window: Window) -> Result<Text, AppError> {
    match reader.peek().map_err(invalid)? {
        ValueType::String => capture(reader, window, 0),
        ValueType::Array => {
            let mut result = Text::default();
            reader.begin_array().map_err(invalid)?;
            while reader.has_next().map_err(invalid)? {
                if reader.peek().map_err(invalid)? != ValueType::Object {
                    reader.skip_value().map_err(invalid)?;
                    continue;
                }
                let mut kind = None;
                let mut text = Text::default();
                object(reader, |name, reader| match name {
                    "type" => string(reader, ID_BYTES).map(|value| kind = value),
                    "text" if reader.peek().map_err(invalid)? == ValueType::String => {
                        capture(reader, window, result.bytes).map(|value| text = value)
                    }
                    _ => reader.skip_value().map_err(invalid),
                })?;
                if matches!(
                    kind.as_deref(),
                    Some("Text" | "text" | "input_text" | "output_text")
                ) {
                    result.bytes = result
                        .bytes
                        .checked_add(text.bytes)
                        .ok_or(AppError::PayloadTooLarge)?;
                    result.picked.extend(text.picked);
                }
            }
            reader.end_array().map_err(invalid)?;
            Ok(result)
        }
        _ => {
            reader.skip_value().map_err(invalid)?;
            Ok(Text::default())
        }
    }
}

fn capture<R: Read>(
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
