//! The app's ledger results mapped field by field to the tools' result DTOs.

use reviewq_app::access::AppError;
use reviewq_app::ledger::passes::owed;
use reviewq_app::ledger::{
    Author, FindingLine, FindingRecord, FinishReceipt, HoldReceipt, LedgerEntries, LedgerPage,
    NoteKey, NoteLine, NoteRecord, NoteSection, OpenPass, RecordReceipt, default_category,
};
use reviewq_domain::identity::GitSha;
use reviewq_domain::ledger::{FindingId, PassKind, PassState};
use reviewq_protocol::ledger as dto;
use reviewq_protocol::reports as report_dto;
use reviewq_protocol::{DiscussionIdDto, FindingIdDto, GitShaDto, ResourceIdDto};

use super::{LedgerResult, cursor};
use crate::entry::ServiceEndpoint;
use crate::entry::features::followups::project_result_kind;
use crate::entry::features::reports::{
    project_basis, project_conclusion, project_finding_section, project_location, project_severity,
};

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
        result: project_result_kind(summary.result),
        new: ids(summary.new)?,
        updated: ids(summary.updated)?,
        retired: ids(summary.retired)?,
        carried: u64::from(summary.carried),
        by_subagents: ids(summary.by_subagents)?,
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
        owes: owed(&pass),
        discussion_ids: pass
            .batch
            .map(|batch| batch.discussion_ids)
            .unwrap_or_default()
            .into_iter()
            .map(|id| DiscussionIdDto::new(id).ok_or(AppError::Internal))
            .collect::<Result<_, _>>()?,
    })
}

fn finding_line(line: FindingLine) -> Result<dto::FindingLine, AppError> {
    Ok(dto::FindingLine {
        id: finding_id(line.id)?,
        revision: line.revision.get(),
        severity: project_severity(line.severity),
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
        severity: project_severity(fields.severity),
        blocking: fields.blocking,
        section: project_finding_section(fields.section),
        category: fields
            .category
            .unwrap_or_else(|| default_category(fields.section).to_owned()),
        locations: body.locations.into_iter().map(project_location).collect(),
        issue: body.issue,
        consequence: body.consequence,
        evidence: project_basis(body.evidence),
        proposed_comment: body.proposed_comment,
        details: body.details,
        outcome: record
            .outcome
            .map(|outcome| {
                Ok::<_, AppError>(dto::RecordedOutcome {
                    outcome: project_conclusion(outcome.conclusion),
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

fn pass_kind(kind: PassKind) -> dto::PassKind {
    match kind {
        PassKind::FullReview => dto::PassKind::FullReview,
        PassKind::SelectedThreads => dto::PassKind::SelectedThreads,
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
