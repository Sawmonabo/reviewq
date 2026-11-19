use std::collections::HashSet;

use reviewq_app::hosts::{Question, QuestionOption};
use reviewq_domain::identity::ReportHash;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAX_QUESTIONS: usize = 16;
const MAX_OPTIONS: usize = 32;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeQuestion {
    id: String,
    header: String,
    question: String,
    options: Option<Vec<NativeOption>>,
    #[serde(default, alias = "is_other")]
    is_other: bool,
    #[serde(default, alias = "is_secret")]
    is_secret: bool,
}

#[derive(Deserialize)]
struct NativeOption {
    label: String,
    description: String,
}

pub(crate) fn project_questions(input: Vec<NativeQuestion>) -> Option<Vec<Question>> {
    if input.is_empty() || input.len() > MAX_QUESTIONS {
        return None;
    }
    let mut seen = HashSet::new();
    let mut questions = Vec::with_capacity(input.len());
    for question in input {
        if !visible(&question.id, 256, false)
            || !seen.insert(question.id.clone())
            || !visible(&question.header, 1024, false)
            || !visible(&question.question, 8192, true)
        {
            return None;
        }
        let options = question.options.unwrap_or_default();
        if options.len() > MAX_OPTIONS {
            return None;
        }
        let mut seen = HashSet::new();
        let mut projected = Vec::with_capacity(options.len());
        for option in options {
            if !visible(&option.label, 1024, false)
                || !seen.insert(option.label.clone())
                || option.description.len() > 4096
            {
                return None;
            }
            projected.push(QuestionOption {
                label: option.label,
                description: option.description,
            });
        }
        questions.push(Question {
            id: question.id,
            header: question.header,
            prompt: question.question,
            options: projected,
            allows_other: question.is_other,
            secret: question.is_secret,
        });
    }
    Some(questions)
}

pub(crate) fn request_id(thread: &str, turn: &str, item: &str) -> String {
    let mut digest = Sha256::new();
    for field in [thread, turn, item] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    ReportHash::from_sha256(digest.finalize().into()).to_string()
}

pub(crate) fn visible(text: &str, limit: usize, multiline: bool) -> bool {
    !text.is_empty()
        && text.len() <= limit
        && !text
            .chars()
            .any(|ch| ch.is_control() && !(multiline && matches!(ch, '\n' | '\r' | '\t')))
}
