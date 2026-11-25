//! The app's ledger results mapped field by field to the tools' result DTOs.

use reviewq_app::access::AppError;
use reviewq_app::followups::ResultKind;
use reviewq_app::ledger::{
    Author, FindingLine, FindingRecord, FinishReceipt, HoldReceipt, LedgerEntries, LedgerPage,
    NoteKey, NoteLine, NoteRecord, NoteSection, OpenPass, RecordReceipt, default_category,
};
use reviewq_app::reports::{
    FindingBasisKind, FindingConclusion, FindingLocation, FindingSection, Severity,
};
use reviewq_domain::identity::GitSha;
use reviewq_domain::ledger::{FindingId, PassKind, PassState};
use reviewq_protocol::followups as followup_dto;
use reviewq_protocol::ledger as dto;
use reviewq_protocol::reports as report_dto;
use reviewq_protocol::{DiscussionIdDto, FindingIdDto, GitShaDto, ResourceIdDto};

use super::{LedgerResult, cursor};
use crate::entry::ServiceEndpoint;

/// The result of an accepted, equal or replayed record write.
pub(super) fn receipt(receipt: RecordReceipt) -> Result<LedgerResult, AppError> {
    Ok(match receipt {
        RecordReceipt::Finding {
            id,
            revision,
            unpublished,
        } => LedgerResult::Finding(dto::FindingReceipt {
            id: finding_id(id)?,
            revision: revision.get(),
            unpublished: u64::from(unpublished),
        }),
        RecordReceipt::Note {
            key,
            revision,
            unpublished,
        } => {
            let (section, title) = note_key(key);
            LedgerResult::Note(dto::NoteReceipt {
                section,
                title,
                revision: revision.get(),
                unpublished: u64::from(unpublished),
            })
        }
    })
}

/// One `review_ledger` page: the short list with the current version and the open pass, or
/// whole records. A further page carries a signed cursor.
pub(super) fn page(
    endpoint: &ServiceEndpoint,
    page: LedgerPage,
) -> Result<dto::LedgerPage, AppError> {
    let cursor = page
        .next
        .map(|next| cursor::encode(endpoint, next))
        .transpose()?;
    Ok(match page.entries {
        LedgerEntries::Short { findings, notes } => dto::LedgerPage::Summary(dto::LedgerSummary {
            receipt_id: page
                .receipt
                .map(|receipt| resource(receipt.as_str()))
                .transpose()?,
            pass: page.pass.map(pass).transpose()?,
            findings: findings
                .into_iter()
                .map(finding_line)
                .collect::<Result<_, _>>()?,
            notes: notes.into_iter().map(note_line).collect(),
            cursor,
        }),
        LedgerEntries::Full { findings, notes } => dto::LedgerPage::Records(dto::LedgerRecords {
            findings: findings
                .into_iter()
                .map(finding_record)
                .collect::<Result<_, _>>()?,
            notes: notes.into_iter().map(note_record).collect(),
            cursor,
        }),
    })
}

/// `finish_review`'s acknowledgement.
pub(super) fn finish(receipt: FinishReceipt) -> Result<dto::FinishReceipt, AppError> {
    let summary = receipt.summary;
    let ids = |ids: Vec<FindingId>| ids.into_iter().map(finding_id).collect::<Result<_, _>>();
    Ok(dto::FinishReceipt {
        receipt_id: resource(receipt.receipt.as_str())?,
        sha256: report_dto::ReportHashDto::new(receipt.sha256.as_str())
            .ok_or(AppError::Internal)?,
        result: match summary.result {
            ResultKind::Final => followup_dto::ResultKind::Final,
            ResultKind::Unchanged => followup_dto::ResultKind::Unchanged,
        },
        new: ids(summary.new)?,
        updated: ids(summary.updated)?,
        retired: ids(summary.retired)?,
        carried: u64::from(summary.carried),
        by_subagents: ids(summary.by_subagents)?,
        empty_sections: summary
            .empty_sections
            .into_iter()
            .map(note_section)
            .collect(),
    })
}

/// `needs_input`'s acknowledgement.
pub(super) fn held(receipt: HoldReceipt) -> dto::HeldReceipt {
    dto::HeldReceipt {
        pass: pass_kind(receipt.pass),
        unpublished: u64::from(receipt.unpublished),
    }
}

fn pass(pass: OpenPass) -> Result<dto::LedgerPass, AppError> {
    Ok(dto::LedgerPass {
        kind: pass_kind(pass.kind),
        waiting: pass.state == PassState::Waiting,
        owes: owes(pass.kind).to_owned(),
        discussion_ids: pass
            .batch
            .map(|batch| batch.discussion_ids)
            .unwrap_or_default()
            .into_iter()
            .map(|id| DiscussionIdDto::new(id).ok_or(AppError::Internal))
            .collect::<Result<_, _>>()?,
    })
}

/// What the closing call of a pass of `kind` must supply.
fn owes(kind: PassKind) -> &'static str {
    match kind {
        PassKind::FullReview => "Call finish_review with your verdict and the reviewed commit.",
        PassKind::SelectedThreads => {
            "Call finish_review with one assessment for each selected discussion."
        }
    }
}

fn finding_line(line: FindingLine) -> Result<dto::FindingLine, AppError> {
    Ok(dto::FindingLine {
        id: finding_id(line.id)?,
        revision: line.revision.get(),
        severity: severity(line.severity),
        blocking: line.blocking,
        title: line.title,
        author: author(line.author),
    })
}

fn note_line(line: NoteLine) -> dto::NoteLine {
    let (section, title) = note_key(line.key);
    dto::NoteLine {
        section,
        title,
        revision: line.revision.get(),
        length: u64::from(line.characters),
    }
}

fn finding_record(record: FindingRecord) -> Result<dto::FindingRecord, AppError> {
    let active = record.is_active();
    let fields = record.fields;
    let body = fields.body;
    Ok(dto::FindingRecord {
        id: finding_id(record.id)?,
        revision: record.revision.get(),
        active,
        author: author(record.author),
        title: fields.title,
        severity: severity(fields.severity),
        blocking: fields.blocking,
        section: section(fields.section),
        category: fields
            .category
            .unwrap_or_else(|| default_category(fields.section).to_owned()),
        locations: body.locations.into_iter().map(location).collect(),
        issue: body.issue,
        consequence: body.consequence,
        evidence: report_dto::FindingBasis {
            kind: match body.evidence.kind {
                FindingBasisKind::Observed => report_dto::FindingBasisKind::Observed,
                FindingBasisKind::Gap => report_dto::FindingBasisKind::Gap,
            },
            text: body.evidence.text,
        },
        proposed_comment: body.proposed_comment,
        details: body.details,
        outcome: record
            .outcome
            .map(|outcome| {
                Ok::<_, AppError>(dto::RecordedOutcome {
                    outcome: conclusion(outcome.conclusion),
                    reason: outcome.reason,
                    discussion_ids: outcome
                        .discussion_ids
                        .into_iter()
                        .map(|id| DiscussionIdDto::new(id).ok_or(AppError::Internal))
                        .collect::<Result<_, _>>()?,
                    checked_sha: outcome.checked_sha.map(git_sha).transpose()?,
                })
            })
            .transpose()?,
    })
}

fn note_record(record: NoteRecord) -> dto::NoteRecord {
    let (section, title) = note_key(record.key);
    dto::NoteRecord {
        section,
        title,
        revision: record.revision.get(),
        body: record.body,
    }
}

/// `lead`, or the subagent's ID.
fn author(author: Author) -> String {
    match author {
        Author::Lead => "lead".to_owned(),
        Author::Subagent(agent) => agent,
    }
}

fn note_key(key: NoteKey) -> (dto::NoteSection, Option<String>) {
    (note_section(key.section()), key.title().map(str::to_owned))
}

fn finding_id(id: FindingId) -> Result<FindingIdDto, AppError> {
    FindingIdDto::new(id.to_string()).ok_or(AppError::Internal)
}

fn resource(value: &str) -> Result<ResourceIdDto, AppError> {
    ResourceIdDto::new(value).ok_or(AppError::Internal)
}

fn git_sha(sha: GitSha) -> Result<GitShaDto, AppError> {
    GitShaDto::new(sha.as_str()).ok_or(AppError::Internal)
}

fn location(value: FindingLocation) -> report_dto::FindingLocation {
    report_dto::FindingLocation {
        path: value.path,
        line: value.line,
    }
}

fn pass_kind(kind: PassKind) -> dto::PassKind {
    match kind {
        PassKind::FullReview => dto::PassKind::FullReview,
        PassKind::SelectedThreads => dto::PassKind::SelectedThreads,
    }
}

fn severity(value: Severity) -> report_dto::Severity {
    match value {
        Severity::P0 => report_dto::Severity::P0,
        Severity::P1 => report_dto::Severity::P1,
        Severity::P2 => report_dto::Severity::P2,
        Severity::P3 => report_dto::Severity::P3,
    }
}

fn section(value: FindingSection) -> report_dto::FindingSection {
    match value {
        FindingSection::Functional => report_dto::FindingSection::Functional,
        FindingSection::Quality => report_dto::FindingSection::Quality,
    }
}

fn conclusion(value: FindingConclusion) -> report_dto::FindingConclusion {
    match value {
        FindingConclusion::Fixed => report_dto::FindingConclusion::Fixed,
        FindingConclusion::ClarifiedWithdrawn => report_dto::FindingConclusion::ClarifiedWithdrawn,
        FindingConclusion::Superseded => report_dto::FindingConclusion::Superseded,
        FindingConclusion::PartlyAddressed => report_dto::FindingConclusion::PartlyAddressed,
        FindingConclusion::StillPresent => report_dto::FindingConclusion::StillPresent,
        FindingConclusion::Unverified => report_dto::FindingConclusion::Unverified,
    }
}

fn note_section(value: NoteSection) -> dto::NoteSection {
    match value {
        NoteSection::Title => dto::NoteSection::Title,
        NoteSection::Summary => dto::NoteSection::Summary,
        NoteSection::Rationale => dto::NoteSection::Rationale,
        NoteSection::Flow => dto::NoteSection::Flow,
        NoteSection::Changes => dto::NoteSection::Changes,
        NoteSection::Threads => dto::NoteSection::Threads,
        NoteSection::Questions => dto::NoteSection::Questions,
        NoteSection::Verification => dto::NoteSection::Verification,
        NoteSection::Publication => dto::NoteSection::Publication,
        NoteSection::Extra => dto::NoteSection::Extra,
    }
}
