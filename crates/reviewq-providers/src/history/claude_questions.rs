use reviewq_app::history::{QuestionOutcome, VisiblePartKind};
use reviewq_app::hosts::Question;
use reviewq_domain::identity::NativeInputId;
use reviewq_domain::session::HostProvider;

use super::file::Lines;
use super::{locator, records};
use crate::question_wire::request_id;

pub(super) const QUESTION_LOOKAHEAD_BYTES: u64 = 4 * 1024 * 1024;
const QUESTION_LOOKAHEAD_LINES: usize = 128;

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
    use std::sync::atomic::AtomicBool;

    use crate::history::file;

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
                        {"header":"Name", "question":"Name the plan?", "multiSelect":true,
                         "options":[{"label":"Pitch"},{"label":"Draft"}]},
                        {"header":"Regions", "question":"Which regions?", "multiSelect":true,
                         "options":[{"label":"North"},{"label":"South"},{"label":"North, South"}]},
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
                "Which lanes?":"North, South", "Name the plan?":"Custom name, with a comma",
                "Which regions?":"North, South", "Private question?":"private answer"
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
        assert_eq!(selected.len(), 3);
        assert_eq!(selected["question-1"], ["North", "South"]);
        assert_eq!(selected["question-2"], ["Custom name, with a comma"]);
        assert_eq!(selected["question-3"], ["North, South"]);
    }
}
