//! Validated tool inputs mapped field by field to the app's requests. The protocol's validator
//! has already refused every shape it states, so a field it guarantees maps to
//! `InvalidRequest` only if the two ever disagree.

use reviewq_app::access::AppError;
use reviewq_app::followups::NextAction;
use reviewq_app::ledger::record::FindingInput;
use reviewq_app::ledger::{
    FindingAssessment, FindingFields, FindingOutcome, FindingPatch, FinishReview, LedgerRead,
    LedgerSelection, NoteKey, NoteSection, NoteWrite,
};
use reviewq_app::reports::{
    Assessment, FindingBasis, FindingBasisKind, FindingConclusion, FindingDetails, FindingLocation,
    FindingSection, RevisionEvidence, Severity, Verdict,
};
use reviewq_domain::ledger::RecordRevision;
use reviewq_protocol::followups as followup_dto;
use reviewq_protocol::ledger as dto;
use reviewq_protocol::reports as report_dto;

use super::cursor;
use crate::entry::ServiceEndpoint;
use crate::entry::pages::parse;

/// `record_finding`: a new finding without `id`, otherwise a patch of the fields given. An
/// empty `proposedComment` or `details` removes the field and `locations: []` clears the
/// locations; a new finding stores neither empty text.
pub(super) fn finding(input: dto::RecordFinding) -> Result<FindingInput, AppError> {
    let Some(id) = input.id else {
        let fields = FindingFields {
            title: given(input.title)?,
            severity: severity(given(input.severity)?),
            blocking: given(input.blocking)?,
            section: section(given(input.section)?),
            category: input.category,
            body: FindingDetails {
                locations: input
                    .locations
                    .unwrap_or_default()
                    .into_iter()
                    .map(location)
                    .collect(),
                issue: given(input.issue)?,
                consequence: given(input.consequence)?,
                evidence: basis(given(input.evidence)?),
                proposed_comment: input.proposed_comment.and_then(written),
                details: input.details.and_then(written),
            },
        };
        return Ok(FindingInput::New(fields));
    };
    Ok(FindingInput::Change {
        id: parse(id.as_str())?,
        expected_revision: revision(given(input.expected_revision)?)?,
        patch: FindingPatch {
            title: input.title,
            severity: input.severity.map(severity),
            blocking: input.blocking,
            section: input.section.map(section),
            category: input.category,
            locations: input
                .locations
                .map(|locations| locations.into_iter().map(location).collect()),
            issue: input.issue,
            consequence: input.consequence,
            evidence: input.evidence.map(basis),
            proposed_comment: input.proposed_comment.map(written),
            details: input.details.map(written),
        },
    })
}

/// `assess_finding`.
pub(super) fn assessment(input: dto::AssessFinding) -> Result<FindingAssessment, AppError> {
    Ok(FindingAssessment {
        id: parse(input.id.as_str())?,
        expected_revision: revision(input.expected_revision)?,
        outcome: FindingOutcome {
            conclusion: conclusion(input.outcome),
            reason: input.reason,
            discussion_ids: input
                .discussion_ids
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            checked_sha: input
                .checked_sha
                .map(|sha| parse(sha.as_str()))
                .transpose()?,
        },
    })
}

/// `record_note`: an `extra` note by its title, any other section by its name.
pub(super) fn note(input: dto::RecordNote) -> Result<NoteWrite, AppError> {
    let key = match note_section(input.section) {
        NoteSection::Extra => NoteKey::extra(given(input.title)?),
        section => NoteKey::fixed(section).ok_or(AppError::InvalidRequest)?,
    };
    Ok(NoteWrite {
        key,
        body: input.body,
        expected_revision: input.expected_revision.map(revision).transpose()?,
    })
}

/// `review_ledger`: the findings named by `ids`, the notes named by `sections`, or the short
/// list when neither is given, continued after `cursor`.
pub(super) fn read(
    endpoint: &ServiceEndpoint,
    input: dto::ReviewLedger,
) -> Result<LedgerRead, AppError> {
    let selection = if !input.ids.is_empty() {
        LedgerSelection::Findings(
            input
                .ids
                .iter()
                .map(|id| parse(id.as_str()))
                .collect::<Result<_, _>>()?,
        )
    } else if !input.sections.is_empty() {
        LedgerSelection::Notes(input.sections.into_iter().map(note_section).collect())
    } else {
        LedgerSelection::Short
    };
    Ok(LedgerRead {
        selection,
        after: input
            .cursor
            .map(|value| cursor::decode(endpoint, &value))
            .transpose()?,
    })
}

/// `finish_review`.
pub(super) fn finish(input: dto::FinishReview) -> Result<FinishReview, AppError> {
    Ok(FinishReview {
        verdict: input.verdict.map(verdict),
        revision: input.revision.map(reviewed_revision).transpose()?,
        full_review: input.full_review,
        assessments: input
            .assessments
            .into_iter()
            .map(|assessment| Assessment {
                discussion_id: assessment.discussion_id.as_str().to_owned(),
                conclusion: conclusion(assessment.conclusion),
                reason: assessment.reason,
                finding_ids: assessment
                    .finding_ids
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect(),
                next_action: match assessment.next_action {
                    followup_dto::NextAction::Review => NextAction::Review,
                    followup_dto::NextAction::WaitingAuthor => NextAction::WaitingAuthor,
                },
            })
            .collect(),
    })
}

/// A field the validator requires in this input.
fn given<T>(value: Option<T>) -> Result<T, AppError> {
    value.ok_or(AppError::InvalidRequest)
}

/// Optional Markdown: empty text is no text.
fn written(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn revision(value: u64) -> Result<RecordRevision, AppError> {
    RecordRevision::try_from(value).map_err(|_| AppError::InvalidRequest)
}

/// `{sha}` is the exact commit and `{sha: null, reason}` an unknown one.
fn reviewed_revision(value: report_dto::ReportRevision) -> Result<RevisionEvidence, AppError> {
    Ok(match value.sha {
        Some(sha) => RevisionEvidence::Exact(parse(sha.as_str())?),
        None => RevisionEvidence::Unknown(given(value.reason)?),
    })
}

fn location(value: report_dto::FindingLocation) -> FindingLocation {
    FindingLocation {
        path: value.path,
        line: value.line,
    }
}

fn basis(value: report_dto::FindingBasis) -> FindingBasis {
    FindingBasis {
        kind: match value.kind {
            report_dto::FindingBasisKind::Observed => FindingBasisKind::Observed,
            report_dto::FindingBasisKind::Gap => FindingBasisKind::Gap,
        },
        text: value.text,
    }
}

fn severity(value: report_dto::Severity) -> Severity {
    match value {
        report_dto::Severity::P0 => Severity::P0,
        report_dto::Severity::P1 => Severity::P1,
        report_dto::Severity::P2 => Severity::P2,
        report_dto::Severity::P3 => Severity::P3,
    }
}

fn section(value: report_dto::FindingSection) -> FindingSection {
    match value {
        report_dto::FindingSection::Functional => FindingSection::Functional,
        report_dto::FindingSection::Quality => FindingSection::Quality,
    }
}

fn conclusion(value: report_dto::FindingConclusion) -> FindingConclusion {
    match value {
        report_dto::FindingConclusion::Fixed => FindingConclusion::Fixed,
        report_dto::FindingConclusion::ClarifiedWithdrawn => FindingConclusion::ClarifiedWithdrawn,
        report_dto::FindingConclusion::Superseded => FindingConclusion::Superseded,
        report_dto::FindingConclusion::PartlyAddressed => FindingConclusion::PartlyAddressed,
        report_dto::FindingConclusion::StillPresent => FindingConclusion::StillPresent,
        report_dto::FindingConclusion::Unverified => FindingConclusion::Unverified,
    }
}

fn verdict(value: dto::ClosingVerdict) -> Verdict {
    match value {
        dto::ClosingVerdict::Approve => Verdict::Approve,
        dto::ClosingVerdict::RequestChanges => Verdict::RequestChanges,
        dto::ClosingVerdict::Blocked => Verdict::Blocked,
    }
}

fn note_section(value: dto::NoteSection) -> NoteSection {
    match value {
        dto::NoteSection::Title => NoteSection::Title,
        dto::NoteSection::Summary => NoteSection::Summary,
        dto::NoteSection::Rationale => NoteSection::Rationale,
        dto::NoteSection::Flow => NoteSection::Flow,
        dto::NoteSection::Changes => NoteSection::Changes,
        dto::NoteSection::Threads => NoteSection::Threads,
        dto::NoteSection::Questions => NoteSection::Questions,
        dto::NoteSection::Verification => NoteSection::Verification,
        dto::NoteSection::Publication => NoteSection::Publication,
        dto::NoteSection::Extra => NoteSection::Extra,
    }
}
