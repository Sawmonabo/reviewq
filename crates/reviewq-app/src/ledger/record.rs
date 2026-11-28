//! The four record tools: `save_finding`, `set_finding_outcome`, `save_section` and `read_draft`.
//! Each checks its Markdown fields with `markdown::check_field`, returns every field error
//! together, then makes one `LedgerStore` call and words a `WriteRefusal`, or a caller the
//! store cannot bind, as a field error. The protocol checked field shapes, sizes and titles;
//! the transport caps the error list. Text of only whitespace counts as empty wherever empty
//! text removes something.

use reviewq_domain::ledger::{FindingId, RecordRevision, Tool};

use super::layout::repeats_fixed_heading;
use super::markdown::{FieldProblem, MarkdownField, check_field};
use super::refusal::for_agent;
use super::required;
use super::{
    CallBinding, FindingAssessment, FindingFields, FindingPatch, LedgerPage, LedgerRead,
    LedgerStore, MAX_ACTIVE_FINDINGS, MAX_EXTRA_NOTES, NoteKey, NoteWrite, RecordField,
    RecordWrite, Recorded, WriteRefusal,
};
use crate::access::{AppError, FieldError};

/// `save_finding`'s input: a new finding, or a change to one already saved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FindingInput {
    /// Saved without `id`. ReviewQ assigns the next unused ID.
    New(FindingFields),
    /// Changes the fields `patch` gives on the finding `id`.
    Change {
        id: FindingId,
        expected_revision: RecordRevision,
        patch: FindingPatch,
    },
}

/// Files or changes one finding. Returns the record's ID and revision, and the run so the
/// caller can settle it. A `proposedComment` or `details` of only whitespace is left out of
/// a new finding and removes the field in a change.
///
/// Fails with `InvalidFields` listing every Markdown field that breaks a rule, with nothing
/// written; or with one field error for a store refusal: an unknown ID, a stale revision, a
/// duplicate of an active finding, a run already at `MAX_ACTIVE_FINDINGS`, or a caller that is
/// neither the run's lead nor a verified child of it. Other store errors pass through.
pub async fn save_finding(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    input: FindingInput,
) -> Result<Recorded, AppError> {
    let (subject, write) = match input {
        FindingInput::New(mut fields) => {
            let body = &mut fields.body;
            body.proposed_comment = body.proposed_comment.take().filter(|text| !blank(text));
            body.details = body.details.take().filter(|text| !blank(text));
            checked(new_finding_errors(&fields))?;
            (
                Subject::NewFinding,
                RecordWrite::CreateFinding(Box::new(fields)),
            )
        }
        FindingInput::Change {
            id,
            expected_revision,
            mut patch,
        } => {
            for field in [&mut patch.proposed_comment, &mut patch.details] {
                if let Some(text) = field
                    && text.as_deref().is_some_and(blank)
                {
                    *text = None;
                }
            }
            checked(patch_errors(&patch))?;
            let write = RecordWrite::ChangeFinding {
                id,
                expected_revision,
                patch: Box::new(patch),
            };
            (Subject::Finding(id), write)
        }
    };
    write_record(store, binding, subject, write).await
}

/// Records what became of a finding already on the run. A retiring outcome removes it from
/// the active set; a keeping outcome on a retired finding reopens it.
///
/// Fails with one field error for an unknown ID, a stale revision, a reopened finding that
/// would take the run past `MAX_ACTIVE_FINDINGS`, or a caller the store cannot bind. Other
/// store errors pass through.
pub async fn set_finding_outcome(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    assessment: FindingAssessment,
) -> Result<Recorded, AppError> {
    let subject = Subject::Finding(assessment.id);
    write_record(
        store,
        binding,
        subject,
        RecordWrite::SetFindingOutcome(assessment),
    )
    .await
}

/// Replaces one note, or removes it with a body that is empty or only whitespace. A removal
/// is not checked for Markdown.
///
/// Fails with `InvalidFields` when the body breaks a Markdown rule for its section, an
/// `extra` note's title repeats a fixed section's heading, or the call removes a required
/// note, which every version must hold, with nothing written; or with one
/// field error for a store refusal: a missing or stale expected revision, removing a note that
/// is not saved, a run already at `MAX_EXTRA_NOTES`, or a caller the store cannot bind.
/// Other store errors pass through.
pub async fn save_section(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    mut note: NoteWrite,
) -> Result<Recorded, AppError> {
    if blank(&note.body) {
        note.body.clear();
    }
    let mut errors = Vec::new();
    if note.key.title().is_some_and(repeats_fixed_heading) {
        errors.push(FieldError::new(
            "/title",
            "This title repeats a heading the report already has. Give the extra section a title of its own, or put the text in the section of that name.",
        ));
    }
    let subject = Subject::Note(note.key.clone());
    if note.body.is_empty() && required::is_required(note.key.section()) {
        errors.push(FieldError::new(
            "/body",
            format!(
                "{} is required in every version of the report, so it cannot be removed. Send its new text to replace it.",
                capitalized(&subject.name()),
            ),
        ));
    }
    if !note.body.is_empty()
        && let Some(field) = MarkdownField::for_note(note.key.section())
    {
        errors.extend(field_error("/body", field, &note.body));
    }
    checked(errors)?;
    write_record(store, binding, subject, RecordWrite::WriteNote(note)).await
}

/// Reads one page of the run's ledger for the calling lead or child. A read is not replayed,
/// so only the binding's session is used.
///
/// Fails with `InvalidFields` for a requested ID that names no finding of the run or a cursor
/// issued for another read, with a field error at `/cursor` when the ledger changed since the
/// cursor was issued, and with a root field error for a caller the store cannot bind. Other
/// store errors pass through.
pub async fn read_draft(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    read: LedgerRead,
) -> Result<LedgerPage, AppError> {
    match store.read_ledger(binding.session.clone(), read).await {
        Ok(page) => Ok(page),
        Err(AppError::CursorExpired) => Err(AppError::InvalidFields(vec![FieldError::new(
            "/cursor",
            "A finding or section was saved or changed after this cursor was issued. Read again without cursor to start from the first page.",
        )])),
        Err(error) => Err(for_agent(store, &binding.session, Tool::ReadDraft, error).await),
    }
}

/// The record a write names, for wording a refusal.
enum Subject {
    NewFinding,
    Finding(FindingId),
    Note(NoteKey),
}

impl Subject {
    /// How a message names the record: `F-03`, `this finding`, `the summary section` or
    /// `the extra section "Rollout"`.
    fn name(&self) -> String {
        match self {
            Self::NewFinding => "this finding".to_owned(),
            Self::Finding(id) => id.to_string(),
            Self::Note(key) => match key.title() {
                Some(title) => format!("the extra section \"{title}\""),
                None => format!("the {} section", key.section()),
            },
        }
    }
}

/// Makes the store call and words its refusal, or a caller it cannot bind, as the one field
/// error of the call.
async fn write_record(
    store: &(impl LedgerStore + ?Sized),
    binding: CallBinding,
    subject: Subject,
    write: RecordWrite,
) -> Result<Recorded, AppError> {
    let session = binding.session.clone();
    let tool = write.tool();
    let sent = sent_revision(&write);
    match store.write_record(binding, write).await {
        Ok(written) => written.map_err(|refusal| {
            AppError::InvalidFields(vec![refusal_error(&refusal, &subject, sent)])
        }),
        Err(error) => Err(for_agent(store, &session, tool, error).await),
    }
}

/// The revision a write names as the one its caller last read.
fn sent_revision(write: &RecordWrite) -> Option<RecordRevision> {
    match write {
        RecordWrite::CreateFinding(_) => None,
        RecordWrite::ChangeFinding {
            expected_revision, ..
        } => Some(*expected_revision),
        RecordWrite::SetFindingOutcome(assessment) => Some(assessment.expected_revision),
        RecordWrite::WriteNote(note) => note.expected_revision,
    }
}

/// Whether authored text is empty or only whitespace, which means no text.
fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// `Ok` when no field broke a rule, otherwise every error at once.
fn checked(errors: Vec<FieldError>) -> Result<(), AppError> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::InvalidFields(errors))
    }
}

/// The Markdown errors of a new finding's fields.
fn new_finding_errors(fields: &FindingFields) -> Vec<FieldError> {
    let body = &fields.body;
    [
        ("/issue", MarkdownField::Issue, Some(&body.issue)),
        (
            "/consequence",
            MarkdownField::Consequence,
            Some(&body.consequence),
        ),
        (
            "/evidence/text",
            MarkdownField::Evidence,
            Some(&body.evidence.text),
        ),
        (
            "/proposedComment",
            MarkdownField::ProposedComment,
            body.proposed_comment.as_ref(),
        ),
        ("/details", MarkdownField::Details, body.details.as_ref()),
    ]
    .into_iter()
    .filter_map(|(path, field, text)| field_error(path, field, text?))
    .collect()
}

/// The Markdown errors of the fields a change gives. A removed field carries no text.
fn patch_errors(patch: &FindingPatch) -> Vec<FieldError> {
    [
        ("/issue", MarkdownField::Issue, patch.issue.as_ref()),
        (
            "/consequence",
            MarkdownField::Consequence,
            patch.consequence.as_ref(),
        ),
        (
            "/evidence/text",
            MarkdownField::Evidence,
            patch.evidence.as_ref().map(|evidence| &evidence.text),
        ),
        (
            "/proposedComment",
            MarkdownField::ProposedComment,
            patch.proposed_comment.as_ref().and_then(Option::as_ref),
        ),
        (
            "/details",
            MarkdownField::Details,
            patch.details.as_ref().and_then(Option::as_ref),
        ),
    ]
    .into_iter()
    .filter_map(|(path, field, text)| field_error(path, field, text?))
    .collect()
}

/// The error of one authored field at `path`, if it breaks a rule. Empty text is a removal
/// and is not checked.
fn field_error(path: &str, field: MarkdownField, text: &str) -> Option<FieldError> {
    if text.is_empty() {
        return None;
    }
    check_field(field, text)
        .err()
        .map(|problem| problem_error(path, problem))
}

/// A Markdown problem as the field error at `path`. The problem's message names its line.
fn problem_error(path: &str, problem: FieldProblem) -> FieldError {
    FieldError::new(path, problem.to_string())
}

/// A store refusal as the field error the agent can act on in the same turn. `sent` is the
/// revision the write named, which a stale-revision refusal compares with the current one.
fn refusal_error(
    refusal: &WriteRefusal,
    subject: &Subject,
    sent: Option<RecordRevision>,
) -> FieldError {
    let name = subject.name();
    let read = Tool::ReadDraft.as_str();
    match refusal {
        WriteRefusal::UnknownFinding => FieldError::new(
            "/id",
            format!(
                "{} is not a finding of this review. Call {read} to see the saved IDs.",
                capitalized(&name),
            ),
        ),
        WriteRefusal::StaleRevision { current, changed } => FieldError::new(
            "/expectedRevision",
            match sent {
                Some(sent) if sent > *current => format!(
                    "{} is at revision {current}, and revision {sent}, which you sent, does not exist yet: send the revision that {read} or a tool result showed you. Read it again with {read}, then send your change with expectedRevision {current}.",
                    capitalized(&name),
                ),
                _ => format!(
                    "{} is at revision {current} now, and {} since the revision you sent. Read it again with {read}, then send your change with expectedRevision {current}.",
                    capitalized(&name),
                    changed_fields(changed),
                ),
            },
        ),
        WriteRefusal::RevisionRequired { current } => FieldError::new(
            "/expectedRevision",
            format!(
                "{} is already saved at revision {current}. Read it with {read}, then send expectedRevision {current} to replace it.",
                capitalized(&name),
            ),
        ),
        WriteRefusal::UnknownNote => FieldError::new(
            "/body",
            format!(
                "{} is not saved, so there is nothing to remove. Send a body to save it.",
                capitalized(&name),
            ),
        ),
        WriteRefusal::DuplicateFinding { id } => FieldError::new(
            "/title",
            format!(
                "{id} is already saved with this title at the same place. Change {id} instead: send its id and the expectedRevision that {read} shows."
            ),
        ),
        WriteRefusal::TooManyFindings => FieldError::new(
            "",
            format!(
                "This review already holds {MAX_ACTIVE_FINDINGS} active findings. Fold this into a related finding, or retire findings that no longer apply with {}.",
                Tool::SetFindingOutcome.as_str()
            ),
        ),
        WriteRefusal::TooManyExtraNotes => FieldError::new(
            "/title",
            format!(
                "This review already holds {MAX_EXTRA_NOTES} extra sections. Add this text to one of them or to another section."
            ),
        ),
    }
}

/// `"title and issue have changed"`, or `"it has changed"` when the store names no field.
fn changed_fields(changed: &[RecordField]) -> String {
    let names: Vec<String> = changed.iter().map(ToString::to_string).collect();
    match names.as_slice() {
        [] => "it has changed".to_owned(),
        [one] => format!("{one} has changed"),
        [first @ .., last] => format!("{} and {last} have changed", first.join(", ")),
    }
}

/// `name` with its first letter in upper case, to start a sentence.
pub(super) fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}
