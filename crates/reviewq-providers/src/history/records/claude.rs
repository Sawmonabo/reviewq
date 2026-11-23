use std::collections::{BTreeMap, HashSet};
use std::io::Read;

use reviewq_app::access::AppError;
use reviewq_app::history::{QuestionOutcome, VisibleKind, VisiblePartKind};
use reviewq_app::hosts::{Question, QuestionOption};
use reviewq_domain::identity::{NativeInputId, NativeSessionId};
use serde::Deserialize;
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use crate::question_wire::{request_id, visible};

use super::{
    BodySink, ID_BYTES, Message, Projection, Text, Window, contents, date, invalid, object, string,
};

pub(in crate::history) const ASK_USER_QUESTION: &str = "AskUserQuestion";
pub(super) const INPUT_WINDOW: Window = Window {
    start: 0,
    limit: 42 * 1024,
    part: None,
};

pub(super) fn project<R: Read>(
    reader: &mut JsonStreamReader<R>,
    native: &NativeSessionId,
    root: Option<&NativeSessionId>,
    window: Window,
    output: &mut BodySink<'_>,
) -> Result<Projection, AppError> {
    let mut item = Message {
        claude: true,
        ..Message::default()
    };
    let mut id = None;
    let mut record_type = None;
    let mut message_role = None;
    let mut stop_reason = None;
    let mut stamp = None;
    let mut hidden = false;
    let mut sidechain = false;
    let mut session = None;
    let mut agent = None;
    let mut secret_answer = false;
    let mut parent = None;
    let mut tool_parent = None;
    object(reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| record_type = value),
        "uuid" => string(reader, ID_BYTES).map(|value| id = value),
        "parentUuid" => string(reader, ID_BYTES).map(|value| parent = value),
        "sourceToolAssistantUUID" => string(reader, ID_BYTES).map(|value| tool_parent = value),
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
            object(reader, |name, reader| match name {
                "role" => string(reader, ID_BYTES).map(|value| message_role = value),
                "stop_reason" => string(reader, ID_BYTES).map(|value| stop_reason = value),
                "content" => contents(reader, window, &mut item, output),
                _ => reader.skip_value().map_err(invalid),
            })
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
    item.final_response =
        record_type.as_deref() == Some("assistant") && stop_reason.as_deref() == Some("end_turn");
    if hidden || !matched {
        return Ok(None);
    }
    if secret_answer {
        item.parts
            .retain(|part| !matches!(part.kind, VisiblePartKind::ToolResult { .. }));
    }
    let kind = match (record_type.as_deref(), message_role.as_deref()) {
        (Some("user"), Some("user")) if item.text.bytes > 0 => VisibleKind::User,
        (Some("user"), Some("user")) => VisibleKind::Assistant,
        (Some("assistant"), Some("assistant")) => VisibleKind::Assistant,
        _ => return Ok(None),
    };
    if record_type.as_deref() == Some("assistant")
        && let Some(id) = id.as_deref()
    {
        for part in &mut item.parts {
            let VisiblePartKind::ToolCall { call, name, .. } = &part.kind else {
                continue;
            };
            if name != ASK_USER_QUESTION {
                continue;
            }
            let questions = input(&part.body).unwrap_or_default();
            part.kind = VisiblePartKind::UserQuestion {
                request: request_id(native.as_str(), id, call.as_str()),
                questions,
                outcome: None,
            };
            part.body = Text::default();
        }
    }
    item.id = id;
    item.claude_tool_parent = tool_parent.or(parent).and_then(|value| value.parse().ok());
    Ok(Some((item, kind, stamp, None)))
}

#[derive(Deserialize)]
struct QuestionInput {
    questions: Vec<NativeQuestion>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeQuestion {
    header: String,
    question: String,
    kind: Option<String>,
    options: Option<Vec<NativeOption>>,
    #[serde(default)]
    multi_select: bool,
    #[serde(default, alias = "is_secret", alias = "secret")]
    is_secret: bool,
}

#[derive(Deserialize)]
struct NativeOption {
    label: String,
    #[serde(default)]
    description: String,
    preview: Option<String>,
}

fn input(body: &Text) -> Option<Vec<Question>> {
    if body.bytes > INPUT_WINDOW.limit as u64 {
        return None;
    }
    let input: QuestionInput = serde_json::from_slice(&body.picked).ok()?;
    if !(1..=4).contains(&input.questions.len()) {
        return None;
    }
    input
        .questions
        .into_iter()
        .enumerate()
        .map(|(index, question)| {
            if !visible(&question.header, 1024, false) || !visible(&question.question, 8192, true) {
                return None;
            }
            let options = question.options.unwrap_or_default();
            match question.kind.as_deref() {
                None | Some("choice") if (2..=4).contains(&options.len()) => (),
                Some("text") if options.is_empty() && !question.multi_select => (),
                _ => return None,
            }
            let mut seen = HashSet::new();
            let options = options
                .into_iter()
                .map(|option| {
                    if !visible(&option.label, 1024, false)
                        || !seen.insert(option.label.clone())
                        || option.description.len() > 4096
                    {
                        return None;
                    }
                    Some(QuestionOption {
                        label: option.label,
                        description: option.description,
                        preview: option
                            .preview
                            .filter(|preview| visible(preview, 8192, true)),
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Question {
                id: format!("question-{}", index + 1),
                header: question.header,
                prompt: question.question,
                options,
                allows_other: true,
                secret: question.is_secret,
                multi_select: question.multi_select,
            })
        })
        .collect()
}

/// An exact native result can close an opening even if its answer was unavailable.
pub(in crate::history) struct QuestionResult {
    pub(super) failed: bool,
    pub(super) answered: bool,
    pub(super) selections: BTreeMap<String, Vec<String>>,
}

impl QuestionResult {
    pub(in crate::history) fn outcome(self) -> Option<QuestionOutcome> {
        if self.failed {
            Some(QuestionOutcome::Closed)
        } else if self.answered {
            Some(QuestionOutcome::Answered(self.selections))
        } else {
            None
        }
    }
}

pub(in crate::history) fn result(
    source: &super::super::locator::Source,
    input: impl Read,
    assistant: &NativeInputId,
    call: &NativeInputId,
    questions: &[Question],
) -> Option<QuestionResult> {
    let mut reader = JsonStreamReader::new(super::NameBudget::new(input));
    let mut record_type = None;
    let mut role = None;
    let mut parent = None;
    let mut session = None;
    let mut agent = None;
    let mut sidechain = false;
    let mut hidden = false;
    let mut matched = None;
    let mut duplicate = false;
    let mut selections = BTreeMap::new();
    let mut answered = false;
    object(&mut reader, |field, reader| match field {
        "type" => string(reader, ID_BYTES).map(|value| record_type = value),
        "sourceToolAssistantUUID" => string(reader, ID_BYTES).map(|value| parent = value),
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
            object(reader, |field, reader| match field {
                "role" => string(reader, ID_BYTES).map(|value| role = value),
                "content" if reader.peek().map_err(invalid)? == ValueType::Array => {
                    reader.begin_array().map_err(invalid)?;
                    while reader.has_next().map_err(invalid)? {
                        let Some(failed) = result_block(reader, call)? else {
                            continue;
                        };
                        duplicate |= matched.replace(failed).is_some();
                    }
                    reader.end_array().map_err(invalid)
                }
                _ => reader.skip_value().map_err(invalid),
            })
        }
        "toolUseResult" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |field, reader| match field {
                "answers" if reader.peek().map_err(invalid)? == ValueType::Object => {
                    object(reader, |prompt, reader| {
                        let mut matches = questions
                            .iter()
                            .filter(|question| question.prompt == prompt);
                        let Some(question) = matches.next().filter(|_| matches.next().is_none())
                        else {
                            return reader.skip_value().map_err(invalid);
                        };
                        let value = string(reader, 8192)?;
                        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                            answered = true;
                            if !question.secret
                                && let Some(labels) = public_choices(question, &value)
                            {
                                selections.insert(question.id.clone(), labels);
                            }
                        }
                        Ok(())
                    })
                }
                "response" => string(reader, 8192)
                    .map(|value| answered |= value.is_some_and(|value| !value.trim().is_empty())),
                _ => reader.skip_value().map_err(invalid),
            })
        }
        _ => reader.skip_value().map_err(invalid),
    })
    .ok()?;
    reader.consume_trailing_whitespace().ok()?;
    let same_session = match &source.root_session {
        Some(root) => {
            sidechain
                && session.as_deref() == Some(root.as_str())
                && agent.as_deref() == Some(source.native_id.as_str())
        }
        None => !sidechain && session.as_deref() == Some(source.native_id.as_str()),
    };
    if duplicate
        || hidden
        || !same_session
        || record_type.as_deref() != Some("user")
        || role.as_deref() != Some("user")
        || parent.as_deref() != Some(assistant.as_str())
    {
        return None;
    }
    Some(QuestionResult {
        failed: matched?,
        answered,
        selections,
    })
}

fn result_block<R: Read>(
    reader: &mut JsonStreamReader<R>,
    call: &NativeInputId,
) -> Result<Option<bool>, AppError> {
    if reader.peek().map_err(invalid)? != ValueType::Object {
        reader.skip_value().map_err(invalid)?;
        return Ok(None);
    }
    let mut kind = None;
    let mut id = None;
    let mut failed = false;
    object(reader, |field, reader| match field {
        "type" => string(reader, ID_BYTES).map(|value| kind = value),
        "tool_use_id" => string(reader, ID_BYTES).map(|value| id = value),
        "is_error" => reader
            .next_bool()
            .map(|value| failed = value)
            .map_err(invalid),
        _ => reader.skip_value().map_err(invalid),
    })?;
    Ok(
        (kind.as_deref() == Some("tool_result") && id.as_deref() == Some(call.as_str()))
            .then_some(failed),
    )
}

fn public_choices(question: &Question, answer: &str) -> Option<Vec<String>> {
    if question.multi_select
        && question
            .options
            .iter()
            .any(|choice| choice.label.contains(','))
    {
        return None;
    }
    let known = |label| question.options.iter().any(|choice| choice.label == label);
    if known(answer) {
        return Some(vec![answer.to_owned()]);
    }
    if !question.multi_select {
        return visible(answer, 8192, true).then(|| vec![answer.to_owned()]);
    }
    let mut labels = Vec::new();
    for label in answer.split(',').map(str::trim) {
        if !known(label) || labels.contains(&label) {
            return None;
        }
        labels.push(label);
    }
    (labels.len() > 1).then(|| labels.into_iter().map(str::to_owned).collect())
}
