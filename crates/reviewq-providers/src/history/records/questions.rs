use std::io::Read;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::{VisibleKind, VisiblePartKind};
use reviewq_app::hosts::Question;
use reviewq_domain::identity::{NativeSessionId, NativeTurnId};
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use crate::question_wire::{AsyncQuestions, NATIVE_QUESTION_TOOL, request_id};

use super::super::{locator::Source, turn_metadata};
use super::{Event, MAX_CONTENT_BLOCKS, Message, Part, Record, Text, Window, invalid, tools};

pub(in crate::history) const NATIVE_QUESTION_TURN_BYTES: u64 = 16 * 1024 * 1024;
const INLINE_QUESTION_BYTES: u64 = 42 * 1024;

pub(super) fn read<R: Read>(
    reader: &mut JsonStreamReader<R>,
) -> Result<Option<Vec<Question>>, AppError> {
    if reader.peek().map_err(invalid)? != ValueType::Array {
        reader.skip_value().map_err(invalid)?;
        return Ok(None);
    }
    let body = tools::body(reader, window(INLINE_QUESTION_BYTES as usize), None)?;
    if body.bytes > INLINE_QUESTION_BYTES {
        // The detailed question endpoint can return an independently verified
        // body. Keep the native row usable within the history page budget.
        return Ok(Some(Vec::new()));
    }
    Ok(serde_json::from_slice::<AsyncQuestions>(&body.picked)
        .ok()
        .and_then(AsyncQuestions::project))
}

pub(super) fn delivery<R: Read>(
    reader: &mut JsonStreamReader<R>,
    item: &mut Message,
) -> Result<(), AppError> {
    let kind = reader.peek().map_err(invalid)?;
    item.delivered = kind != ValueType::Null;
    if kind == ValueType::String {
        let body = tools::body(reader, window(5), None)?;
        item.async_delivery = body.bytes == 5 && body.picked == b"async";
        Ok(())
    } else {
        reader.skip_value().map_err(invalid)
    }
}

pub(super) fn attach(event: &mut Event, thread: &NativeSessionId, first_part: u16) {
    let item = &mut event.item;
    let (Some(turn), Some(id)) = (event.turn.as_deref(), item.id.as_deref()) else {
        return;
    };
    if !item.async_delivery || item.moderated || turn.parse::<NativeTurnId>().is_err() {
        return;
    }
    if let Some(mut questions) = item.questions.take()
        && item.next_part.is_none()
        && first_part <= MAX_CONTENT_BLOCKS
    {
        if first_part == 0 && item.parts.is_empty() && item.text.bytes > 0 {
            item.parts.push(Part {
                index: 0,
                kind: VisiblePartKind::Text(VisibleKind::Assistant),
                body: item.text.clone(),
            });
        }
        questions.retain(|question| !question.secret);
        item.parts.push(Part {
            index: MAX_CONTENT_BLOCKS,
            kind: VisiblePartKind::UserQuestion {
                request: request_id(thread.as_str(), turn, id),
                questions,
            },
            body: Text::default(),
        });
    }
}

pub(super) fn native_opening(event: &mut Event, thread: &NativeSessionId) -> Option<Message> {
    if event.kind.as_deref() != Some("function_call")
        || event.tool.as_deref() != Some(NATIVE_QUESTION_TOOL)
        || event
            .thread
            .as_deref()
            .is_some_and(|id| id != thread.as_str())
        || event
            .turn
            .as_deref()
            .is_some_and(|turn| turn.parse::<NativeTurnId>().is_err())
    {
        return None;
    }
    Some(Message {
        id: event.call.take(),
        native_question: true,
        ..Message::default()
    })
}

pub(in crate::history) fn bind_native_question(
    record: &mut Record,
    source: &Source,
    offset: u64,
    budget: &mut u64,
    cancel: &AtomicBool,
) -> bool {
    if !record.native_question {
        return true;
    }
    let Ok(Some(turn)) = turn_metadata::turn_before(source, offset, budget, cancel) else {
        return false;
    };
    if record
        .turn
        .as_ref()
        .is_some_and(|explicit| explicit.as_str() != turn)
    {
        return false;
    }
    let Ok(turn) = turn.parse::<NativeTurnId>() else {
        return false;
    };
    record.parts.push(Part {
        index: 0,
        kind: VisiblePartKind::UserQuestion {
            request: request_id(source.native_id.as_str(), turn.as_str(), record.id.as_str()),
            questions: Vec::new(),
        },
        body: Text::default(),
    });
    record.turn = Some(turn);
    true
}

fn window(limit: usize) -> Window {
    Window {
        start: 0,
        limit,
        part: None,
    }
}
