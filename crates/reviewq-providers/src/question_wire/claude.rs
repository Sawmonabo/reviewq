use std::collections::HashSet;

use reviewq_app::hosts::{Question, QuestionOption};
use serde::Deserialize;

use super::visible;

pub(crate) const MAX_INPUT_BYTES: usize = 42 * 1024;

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

pub(crate) fn project(input: &[u8]) -> Option<Vec<Question>> {
    if input.len() > MAX_INPUT_BYTES {
        return None;
    }
    let input: QuestionInput = serde_json::from_slice(input).ok()?;
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
