use std::io::Read;

use reviewq_app::access::AppError;
use reviewq_app::history::{VisibleEntry, VisibleKind};
use reviewq_domain::identity::NativeSessionId;
use reviewq_domain::session::HostProvider;
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

const ID_BYTES: u64 = 512;
const PREVIEW_CHARS: usize = 512;
const PREVIEW_BYTES: u64 = 4 * PREVIEW_CHARS as u64 + 4;

#[derive(Default)]
struct Message {
    id: Option<String>,
    kind: Option<String>,
    text: String,
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
    position: u64,
    native: &NativeSessionId,
) -> Result<Option<VisibleEntry>, AppError> {
    // A malformed complete record does not certify text or native identities.
    let mut reader = JsonStreamReader::new(input);
    let result = match provider {
        HostProvider::Codex => codex(&mut reader, native),
        HostProvider::Claude => claude(&mut reader),
    };
    let Ok(Some((item, kind, stamp, turn))) = result else {
        return Ok(None);
    };
    if reader.consume_trailing_whitespace().is_err() {
        return Ok(None);
    }
    let Some(id) = item.id.and_then(|value| value.parse().ok()) else {
        return Ok(None);
    };
    if item.text.is_empty() {
        return Ok(None);
    }
    Ok(Some(VisibleEntry {
        id,
        turn: turn.and_then(|value| value.parse().ok()),
        at_ms: stamp,
        kind,
        text: item.text,
        record_offset: Some(position),
    }))
}

type Projection = Option<(Message, VisibleKind, Option<i64>, Option<String>)>;

fn codex<R: Read>(
    reader: &mut JsonStreamReader<R>,
    native: &NativeSessionId,
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
                    message(reader).map(|value| event.item = value)
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

fn claude<R: Read>(reader: &mut JsonStreamReader<R>) -> Result<Projection, AppError> {
    let mut item = Message::default();
    let mut id = None;
    let mut role = None;
    let mut stamp = None;
    let mut hidden = false;
    object(reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| role = value),
        "uuid" => string(reader, ID_BYTES).map(|value| id = value),
        "timestamp" => string(reader, ID_BYTES).map(|value| stamp = date(value)),
        "isMeta" | "isCompactSummary" | "isVisibleInTranscriptOnly"
            if reader.peek().map_err(invalid)? == ValueType::Boolean =>
        {
            reader
                .next_bool()
                .map(|value| hidden |= value)
                .map_err(invalid)
        }
        "message" if reader.peek().map_err(invalid)? == ValueType::Object => {
            message(reader).map(|value| item = value)
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    if hidden {
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

fn message<R: Read>(reader: &mut JsonStreamReader<R>) -> Result<Message, AppError> {
    let mut item = Message::default();
    object(reader, |name, reader| match name {
        "type" | "role" => string(reader, ID_BYTES).map(|value| item.kind = value),
        "id" => string(reader, ID_BYTES).map(|value| item.id = value),
        "content" => contents(reader, &mut item.text),
        _ => reader.skip_value().map_err(invalid),
    })?;
    Ok(item)
}

fn contents<R: Read>(
    reader: &mut JsonStreamReader<R>,
    output: &mut String,
) -> Result<(), AppError> {
    match reader.peek().map_err(invalid)? {
        ValueType::String => {
            let content = preview(reader)?;
            append(output, &content);
            Ok(())
        }
        ValueType::Array => {
            reader.begin_array().map_err(invalid)?;
            while reader.has_next().map_err(invalid)? {
                if reader.peek().map_err(invalid)? != ValueType::Object {
                    reader.skip_value().map_err(invalid)?;
                    continue;
                }
                let mut kind = None;
                let mut text = String::new();
                object(reader, |name, reader| match name {
                    "type" => string(reader, ID_BYTES).map(|value| kind = value),
                    "text" => preview(reader).map(|value| text = value),
                    _ => reader.skip_value().map_err(invalid),
                })?;
                if matches!(
                    kind.as_deref(),
                    Some("Text" | "text" | "input_text" | "output_text")
                ) {
                    append(output, &text);
                }
            }
            reader.end_array().map_err(invalid)
        }
        _ => reader.skip_value().map_err(invalid),
    }
}

fn append(output: &mut String, value: &str) {
    let remaining = PREVIEW_CHARS.saturating_sub(output.chars().count());
    output.extend(value.chars().take(remaining));
}

fn preview<R: Read>(reader: &mut JsonStreamReader<R>) -> Result<String, AppError> {
    // The JSON reader validates and skips the rest of the selected string without retaining it.
    if reader.peek().map_err(invalid)? != ValueType::String {
        reader.skip_value().map_err(invalid)?;
        return Ok(String::new());
    }
    let bytes = prefix(reader, PREVIEW_BYTES)?;
    let usable = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).map_err(invalid)?
        }
        Err(_) => return Err(AppError::Unavailable),
    };
    Ok(usable.chars().take(PREVIEW_CHARS).collect())
}

fn string<R: Read>(
    reader: &mut JsonStreamReader<R>,
    limit: u64,
) -> Result<Option<String>, AppError> {
    if reader.peek().map_err(invalid)? != ValueType::String {
        reader.skip_value().map_err(invalid)?;
        return Ok(None);
    }
    let bytes = prefix(reader, limit + 1)?;
    if bytes.len() as u64 > limit {
        return Err(AppError::Unavailable);
    }
    String::from_utf8(bytes).map(Some).map_err(invalid)
}

fn prefix<R: Read>(reader: &mut JsonStreamReader<R>, limit: u64) -> Result<Vec<u8>, AppError> {
    let mut text = reader.next_string_reader().map_err(invalid)?;
    let mut bytes = Vec::new();
    (&mut text)
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    std::io::copy(&mut text, &mut std::io::sink()).map_err(invalid)?;
    Ok(bytes)
}

fn object<R: Read>(
    reader: &mut JsonStreamReader<R>,
    mut field: impl FnMut(&str, &mut JsonStreamReader<R>) -> Result<(), AppError>,
) -> Result<(), AppError> {
    reader.begin_object().map_err(invalid)?;
    while reader.has_next().map_err(invalid)? {
        let name = reader.next_name_owned().map_err(invalid)?;
        if name.len() > 256 {
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

fn invalid<T>(_: T) -> AppError {
    AppError::Unavailable
}
