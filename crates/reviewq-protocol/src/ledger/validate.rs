//! The one check of a reviewing tool input's shape and size. The app checks the Markdown, the
//! report layout and the per-run limits, and the store checks revisions and duplicates.

use std::collections::BTreeSet;
use std::io;
use std::sync::LazyLock;

use jsonschema::{Keyword, ValidationError, Validator, paths::Location};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::inputs::{
    LedgerInput, MAX_DETAILS_BYTES, MAX_NOTE_BODY_BYTES, MAX_TITLE_CHARS, MAX_TOOL_INPUT_BYTES,
    ReadDraft, SaveFinding, SaveSection, SetFindingOutcome, SubmitReview, WaitForUser,
};
use super::{LEDGER_TOOLS, LedgerTool};
use crate::field_errors::{FieldError, collect_schema_errors};
use crate::reports::MAX_FINDING_TEXT_BYTES;

/// The fields a finding saved without `id` must give.
const NEW_FINDING_FIELDS: [&str; 7] = [
    "title",
    "severity",
    "blocking",
    "kind",
    "issue",
    "consequence",
    "evidence",
];

/// Byte limits of the Markdown fields. The schema states each as a character maximum.
const FINDING_TEXT_LIMITS: [(&str, usize); 5] = [
    ("/issue", MAX_FINDING_TEXT_BYTES),
    ("/consequence", MAX_FINDING_TEXT_BYTES),
    ("/evidence/text", MAX_FINDING_TEXT_BYTES),
    ("/proposedComment", MAX_FINDING_TEXT_BYTES),
    ("/details", MAX_DETAILS_BYTES),
];

static VALIDATORS: LazyLock<[(LedgerTool, Validator); 6]> = LazyLock::new(|| {
    LEDGER_TOOLS.map(|tool| {
        let validator = compile(&Value::Object(tool.input_schema().clone()))
            .expect("the generated ledger tool schemas compile");
        (tool, validator)
    })
});

/// Checks `input` for `tool` and returns it typed. A refusal lists every field that breaks a
/// rule, at its JSON Pointer within `input`, in field order and uncapped: the transport caps
/// the reply with `bounded_field_errors`. It reads no stored state, so a valid input can still
/// be refused later.
pub fn validate_ledger_input(
    tool: LedgerTool,
    input: &Value,
) -> Result<LedgerInput, Vec<FieldError>> {
    let (_, validator) = VALIDATORS
        .iter()
        .find(|(compiled, _)| *compiled == tool)
        .expect("every ledger tool has a validator");
    check(tool, validator, input)
}

/// Checks `input` against a freshly generated `schema` for `tool`, so the schema exporter can
/// prove its example before the schema is checked in.
#[cfg(feature = "schema")]
pub fn validate_against(
    schema: &Value,
    tool: LedgerTool,
    input: &Value,
) -> Result<LedgerInput, Vec<FieldError>> {
    let validator = compile(schema).map_err(|error| {
        vec![FieldError::new(
            "",
            format!("The schema does not compile: {error}"),
        )]
    })?;
    check(tool, &validator, input)
}

/// The refusal of an input larger than `MAX_TOOL_INPUT_BYTES` as compact JSON. A host that
/// cannot carry such an input as far as the validator refuses it with this same error.
pub fn oversized_input() -> FieldError {
    FieldError::new(
        "",
        format!(
            "The input is larger than {} KiB, so shorten the longest fields.",
            MAX_TOOL_INPUT_BYTES / 1024
        ),
    )
}

fn compile(schema: &Value) -> Result<Validator, ValidationError<'static>> {
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .with_keyword("maxUtf8Bytes", max_utf8_bytes)
        .build(schema)
}

fn check(
    tool: LedgerTool,
    validator: &Validator,
    input: &Value,
) -> Result<LedgerInput, Vec<FieldError>> {
    if compact_length(input) > MAX_TOOL_INPUT_BYTES {
        return Err(vec![oversized_input()]);
    }
    let mut errors = BTreeSet::new();
    collect_schema_errors(validator, input, &mut errors);
    if let Some(object) = input.as_object() {
        let mut rules = Rules {
            input: object,
            errors: &mut errors,
        };
        match tool {
            LedgerTool::SaveFinding => rules.save_finding(),
            LedgerTool::SetFindingOutcome => {
                rules.one_line("/reason");
            }
            LedgerTool::SaveSection => rules.save_section(),
            LedgerTool::ReadDraft => rules.read_draft(),
            LedgerTool::SubmitReview => rules.submit_review(),
            LedgerTool::WaitForUser => {
                rules.written("/question");
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors.into_iter().collect());
    }
    decode(tool, input).map_err(|_| {
        vec![FieldError::new(
            "",
            "The input does not match the tool schema, so check each field and call again.",
        )]
    })
}

fn decode(tool: LedgerTool, input: &Value) -> Result<LedgerInput, serde_json::Error> {
    Ok(match tool {
        LedgerTool::SaveFinding => LedgerInput::SaveFinding(SaveFinding::deserialize(input)?),
        LedgerTool::SetFindingOutcome => {
            LedgerInput::SetFindingOutcome(SetFindingOutcome::deserialize(input)?)
        }
        LedgerTool::SaveSection => LedgerInput::SaveSection(SaveSection::deserialize(input)?),
        LedgerTool::ReadDraft => LedgerInput::ReadDraft(ReadDraft::deserialize(input)?),
        LedgerTool::SubmitReview => LedgerInput::SubmitReview(SubmitReview::deserialize(input)?),
        LedgerTool::WaitForUser => LedgerInput::WaitForUser(WaitForUser::deserialize(input)?),
    })
}

/// The rules a flat schema cannot state: fields required or excluded by another field, text
/// the reader cannot show, one-line and blank text, the title rule and the byte limits.
struct Rules<'a> {
    input: &'a Map<String, Value>,
    errors: &'a mut BTreeSet<FieldError>,
}

impl Rules<'_> {
    fn save_finding(&mut self) {
        let input = self.input;
        let given = |field: &str| input.contains_key(field);
        if given("id") {
            if !given("expectedRevision") {
                self.refuse(
                    "/expectedRevision",
                    "Required with id: give the revision you last read.",
                );
            }
            if input.len() <= 2 && given("expectedRevision") {
                self.refuse("", "Give at least one field to change.");
            }
        } else {
            for field in NEW_FINDING_FIELDS.into_iter().filter(|field| !given(field)) {
                self.refuse(&format!("/{field}"), "Required for a new finding.");
            }
            if given("expectedRevision") {
                self.refuse(
                    "/expectedRevision",
                    "Leave this out for a new finding, or give the finding's id to change it.",
                );
            }
        }
        if self.one_line("/title") && self.text("/title").is_some_and(starts_with_severity) {
            self.refuse(
                "/title",
                "Leave the severity label out of the title, since severity has its own field.",
            );
        }
        if self.one_line("/category")
            && self
                .text("/category")
                .is_some_and(|text| text.contains('·'))
        {
            self.refuse("/category", "Leave out the · character.");
        }
        let locations = self
            .input
            .get("locations")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        for index in 0..locations {
            self.one_line(&format!("/locations/{index}/path"));
        }
        for (pointer, limit) in FINDING_TEXT_LIMITS {
            self.readable(pointer);
            self.bytes(pointer, limit);
        }
    }

    fn save_section(&mut self) {
        let extra = self.text("/section") == Some("extra");
        match (extra, self.input.contains_key("title")) {
            (true, false) => self.refuse("/title", "Required for an extra section."),
            (false, true) => self.refuse("/title", "Give title only for an extra section."),
            _ => {}
        }
        self.one_line("/title");
        // A body of only whitespace removes the section, so it is not held to the title's shape.
        if self.readable("/body") && self.text("/section") == Some("title") {
            let body = self.text("/body").unwrap_or_default();
            let removes = body.trim().is_empty();
            if !removes && (body.chars().any(breaks_line) || body.chars().count() > MAX_TITLE_CHARS)
            {
                self.refuse(
                    "/body",
                    &format!(
                        "Write the title section on one line of at most {MAX_TITLE_CHARS} characters, or leave it empty to remove it."
                    ),
                );
            }
        }
        self.bytes("/body", MAX_NOTE_BODY_BYTES);
    }

    fn read_draft(&mut self) {
        let listed = |field: &str| {
            self.input
                .get(field)
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
        };
        if listed("ids") && listed("sections") {
            self.refuse(
                "/sections",
                "Give ids or sections, not both: read each in its own call.",
            );
        }
    }

    fn submit_review(&mut self) {
        if let Some(revision) = self.input.get("revision").and_then(Value::as_object) {
            match (revision.get("sha"), revision.contains_key("reason")) {
                (Some(Value::Null), false) => {
                    self.refuse(
                        "/revision/reason",
                        "Required when sha is null: say why the reviewed commit is unknown.",
                    );
                }
                (Some(Value::String(_)), true) => {
                    self.refuse("/revision/reason", "Leave out reason when sha is given.");
                }
                _ => {}
            }
            self.one_line("/revision/reason");
        }
        let assessments = self
            .input
            .get("assessments")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        for index in 0..assessments {
            self.written(&format!("/assessments/{index}/reason"));
        }
    }

    /// Refuses unreadable characters, a line break, a control character or blank text at
    /// `pointer`. Returns whether the field is absent or passed.
    fn one_line(&mut self, pointer: &str) -> bool {
        if !self.written(pointer) {
            return false;
        }
        if self
            .text(pointer)
            .is_some_and(|text| text.chars().any(breaks_line))
        {
            self.refuse(
                pointer,
                "Write this on one line, without line breaks or control characters.",
            );
            return false;
        }
        true
    }

    /// Refuses unreadable characters or text of only spaces at `pointer`. An empty string is
    /// left to the schema. Returns whether the field is absent or passed.
    fn written(&mut self, pointer: &str) -> bool {
        if !self.readable(pointer) {
            return false;
        }
        if self
            .text(pointer)
            .is_some_and(|text| !text.is_empty() && text.trim().is_empty())
        {
            self.refuse(pointer, "Write words here, not only spaces.");
            return false;
        }
        true
    }

    /// Refuses a character that breaks the reader at `pointer`: a byte-order mark, a NUL or a
    /// carriage return without a line feed after it. CRLF line endings pass. Returns whether
    /// the field is absent or passed.
    fn readable(&mut self, pointer: &str) -> bool {
        let Some(problem) = self.text(pointer).and_then(unreadable) else {
            return true;
        };
        self.refuse(pointer, problem);
        false
    }

    fn bytes(&mut self, pointer: &str, limit: usize) {
        // Text over the limit in characters already has the schema's error.
        if self
            .text(pointer)
            .is_some_and(|text| text.len() > limit && text.chars().count() <= limit)
        {
            self.refuse(
                pointer,
                &format!(
                    "Shorten this to at most {} KiB of UTF-8 text.",
                    limit / 1024
                ),
            );
        }
    }

    fn text(&self, pointer: &str) -> Option<&str> {
        let mut parts = pointer.strip_prefix('/')?.split('/');
        let first = self.input.get(parts.next()?)?;
        parts
            .try_fold(first, |value, part| match value {
                Value::Object(object) => object.get(part),
                Value::Array(items) => items.get(part.parse::<usize>().ok()?),
                _ => None,
            })?
            .as_str()
    }

    fn refuse(&mut self, pointer: &str, message: &str) {
        self.errors.insert(FieldError::new(pointer, message));
    }
}

/// Whether a title opens with a severity label such as `P1 –` or `[P0]`.
fn starts_with_severity(title: &str) -> bool {
    let rest = title.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '[' | '(' | '*' | '_' | '`')
    });
    let mut chars = rest.chars();
    matches!(chars.next(), Some('P' | 'p'))
        && matches!(chars.next(), Some('0'..='3'))
        && chars.next().is_none_or(|c| !c.is_alphanumeric())
}

/// What to remove from `text` so the reader can show it, if anything.
fn unreadable(text: &str) -> Option<&'static str> {
    if text.contains('\u{feff}') {
        Some("Remove the byte-order mark (U+FEFF) from this text.")
    } else if text.contains('\0') {
        Some("Remove the NUL character (U+0000) from this text.")
    } else if text
        .match_indices('\r')
        .any(|(index, _)| !text[index + 1..].starts_with('\n'))
    {
        Some("Remove each carriage return (U+000D) that no line feed follows.")
    } else {
        None
    }
}

fn breaks_line(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')
}

/// The input's size as compact JSON, counted without building the text.
fn compact_length(input: &Value) -> usize {
    struct Count(usize);
    impl io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, input).map_or(usize::MAX, |()| count.0)
}

/// Enforces the scalar DTOs' `maxUtf8Bytes`, which JSON Schema itself does not define.
fn max_utf8_bytes<'a>(
    _parent: &'a Map<String, Value>,
    value: &'a Value,
    _path: Location,
) -> Result<Box<dyn for<'i> Keyword<'i>>, ValidationError<'a>> {
    let limit = value
        .as_u64()
        .and_then(|limit| usize::try_from(limit).ok())
        .ok_or_else(|| ValidationError::schema("maxUtf8Bytes must be a nonnegative integer"))?;
    Ok(Box::new(MaxUtf8Bytes(limit)))
}

struct MaxUtf8Bytes(usize);

impl<'i> Keyword<'i> for MaxUtf8Bytes {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ValidationError::custom(format!(
                "Use at most {} bytes.",
                self.0
            )))
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        instance.as_str().is_none_or(|text| text.len() <= self.0)
    }
}
