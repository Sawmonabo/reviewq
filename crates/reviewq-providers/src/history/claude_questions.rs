use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::{HistorySession, QuestionOutcome, VisiblePartKind};
use reviewq_app::hosts::{ClaudeQuestionAddress, Question};
use reviewq_domain::identity::NativeInputId;
use reviewq_domain::session::HostProvider;

use super::file::{self, Lines};
use super::{locator, records};
use crate::question_wire::request_id;

pub(super) const QUESTION_LOOKAHEAD_BYTES: u64 = 4 * 1024 * 1024;
const QUESTION_LOOKAHEAD_LINES: usize = 128;

pub(crate) fn open(
    session: &HistorySession,
    address: &ClaudeQuestionAddress,
    cancel: &AtomicBool,
) -> Result<Vec<Question>, AppError> {
    if session.parent.is_some() || session.binding.identity().provider() != HostProvider::Claude {
        return Err(AppError::Forbidden);
    }
    let source = locator::locate(session, cancel)?.ok_or(AppError::NotFound)?;
    let length = source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len();
    let mut budget = QUESTION_LOOKAHEAD_BYTES;
    let complete = file::end_of_line(&source.file, length, &mut budget, cancel)?;
    let mut lines = Lines::new(&source.file, cancel);
    let mut end = complete;
    let request = request_id(
        source.native_id.as_str(),
        address.assistant.as_str(),
        address.call.as_str(),
    );
    for _ in 0..QUESTION_LOOKAHEAD_LINES {
        if end == 0 || budget == 0 {
            break;
        }
        let start = lines.previous(end - 1, &mut budget)?;
        let record = records::project(
            HostProvider::Claude,
            lines.input(start, end),
            &source.native_id,
            None,
            0,
            0,
        );
        if let Some(record) = record
            && record.id == address.assistant
        {
            let mut matching = record.parts.into_iter().filter_map(|part| match part.kind {
                VisiblePartKind::UserQuestion {
                    request: candidate,
                    questions,
                    ..
                } if candidate == request => Some(questions),
                _ => None,
            });
            let questions = matching.next().ok_or(AppError::NotFound)?;
            let mut prompts = HashSet::new();
            if matching.next().is_some()
                || questions.is_empty()
                || questions
                    .iter()
                    .any(|question| question.secret || !prompts.insert(&question.prompt))
            {
                return Err(AppError::Unavailable);
            }
            if outcome(
                &source,
                &mut lines,
                end,
                complete,
                &mut budget,
                &address.assistant,
                &address.call,
                &questions,
            )
            .is_some()
            {
                return Err(AppError::StaleRevision);
            }
            return Ok(questions);
        }
        end = start;
    }
    Err(AppError::NotFound)
}

pub(super) fn attach_outcomes(
    source: &locator::Source,
    lines: &mut Lines<'_>,
    end: u64,
    complete: u64,
    budget: &mut u64,
    record: &mut records::Record,
) {
    if source.provider != HostProvider::Claude {
        return;
    }
    for part in &mut record.parts {
        let VisiblePartKind::UserQuestion {
            request,
            questions,
            outcome: result,
        } = &mut part.kind
        else {
            continue;
        };
        let mut matching = record.claude_tool_calls.iter().filter(|(call, name)| {
            name == records::ASK_USER_QUESTION
                && *request
                    == request_id(source.native_id.as_str(), record.id.as_str(), call.as_str())
        });
        if let Some((call, _)) = matching.next()
            && matching.next().is_none()
        {
            *result = outcome(
                source, lines, end, complete, budget, &record.id, call, questions,
            )
            .flatten();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn outcome(
    source: &locator::Source,
    lines: &mut Lines<'_>,
    mut start: u64,
    complete: u64,
    budget: &mut u64,
    assistant: &NativeInputId,
    call: &NativeInputId,
    questions: &[Question],
) -> Option<Option<QuestionOutcome>> {
    for _ in 0..QUESTION_LOOKAHEAD_LINES {
        if start >= complete || *budget == 0 {
            break;
        }
        let end = lines.next(start, complete, budget).ok()?;
        if let Some(result) = records::claude_question_result(
            source,
            lines.input(start, end),
            assistant,
            call,
            questions,
        ) {
            return Some(result.outcome());
        }
        start = end;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use reviewq_app::history::NativePosition;
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn history_uses_the_exact_visible_opening_and_result() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let opening = |id, kind| {
            json!({
                "type": kind, "uuid": id, "sessionId": "session", "isSidechain": false,
                "message": {"role": "assistant", "content": [{
                    "type": "tool_use", "name": "AskUserQuestion", "id": "repeated-call",
                    "input": {"questions": [
                        {"header":"Lanes", "question":"Which lanes?", "multiSelect":true,
                         "options":[{"label":"North", "preview":"North diagram"},{"label":"South"}]},
                        {"header":"Name", "question":"Name the plan?",
                         "options":[{"label":"Pitch"},{"label":"Draft"}]},
                        {"header":"Private", "question":"Private question?", "isSecret":true,
                         "options":[{"label":"One"},{"label":"Two"}]}
                    ]}
                }]}
            })
        };
        let result = |id, assistant, failed, candidates: Value| {
            json!({
                "type": "user", "uuid": id, "sessionId": "session", "isSidechain": false,
                "sourceToolAssistantUUID": assistant,
                "message": {"role": "user", "content": [{"type":"tool_result", "tool_use_id":"repeated-call",
                             "is_error": failed, "content":"unprojected provider body"}]},
                "toolUseResult": candidates,
            })
        };
        for row in [
            opening("first", "assistant"),
            opening("copy", "api-request-blob"),
            result("unrelated", "other", true, Value::Null),
            result(
                "first-result",
                "first",
                false,
                json!({"answers":{
                "Which lanes?":"North, South", "Name the plan?":"Custom name", "Private question?":"private answer"
            }, "annotations":{"Name the plan?":"unprojected annotation"}}),
            ),
            opening("second", "assistant"),
            result("second-result", "second", true, Value::Null),
            opening("third", "assistant"),
        ] {
            writeln!(file, "{row}").unwrap();
        }
        let source = locator::Source {
            file: file.reopen().unwrap(),
            provider: HostProvider::Claude,
            native_id: "session".parse().unwrap(),
            fingerprint: [0; 32],
            root_session: None,
            path: file.path().to_path_buf(),
        };
        let page = file::page(
            &source,
            NativePosition::default(),
            12,
            &AtomicBool::new(false),
        )
        .unwrap();
        let mut forms = Vec::new();
        for entry in &page.entries {
            for part in &entry.parts {
                assert!(!part.text.contains("unprojected"));
                if let VisiblePartKind::UserQuestion {
                    request,
                    questions,
                    outcome,
                } = &part.kind
                {
                    assert!(part.content.is_none());
                    forms.push((request, questions, outcome));
                }
            }
        }
        let [
            (first, questions, Some(QuestionOutcome::Answered(selected))),
            (second, _, Some(QuestionOutcome::Closed)),
            (third, _, None),
        ] = forms.as_slice()
        else {
            panic!("one answered, one closed, and one unresolved exact question");
        };
        assert_ne!(first, second);
        assert_ne!(second, third);
        assert!(questions[0].multi_select);
        assert_eq!(
            questions[0].options[0].preview.as_deref(),
            Some("North diagram")
        );
        assert_eq!(selected.len(), 2);
        assert_eq!(selected["question-1"], ["North", "South"]);
        assert_eq!(selected["question-2"], ["Custom name"]);
    }
}
