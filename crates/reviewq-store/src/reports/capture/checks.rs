//! Field errors a handoff would otherwise meet at acceptance, found while its tool call is
//! open so the agent corrects them in the same turn.

use reviewq_app::reports::{
    Declaration, HandoffFieldError, METADATA_BYTES, RevisionEvidence, ledger,
};
use rusqlite::Connection;

use super::{batch, structured};
use crate::StoreError;

/// Checks the ledger against the head's active findings and a bound batch's frozen
/// manifest. Returns the errors and, for a final with a ledger, the projection whose
/// metadata the size check measures once the handoff's stored form is known.
pub(super) fn ledger(
    db: &Connection,
    run: &str,
    head: Option<&str>,
    batch: Option<&str>,
    declaration: &Declaration,
    headings: Option<&[String]>,
) -> Result<(Vec<HandoffFieldError>, Option<structured::Projection>), StoreError> {
    let mut errors = Vec::new();
    let manifest = batch.map(|id| batch::frozen(db, id)).transpose()?;
    if let Some(manifest) = &manifest {
        let assessments = declaration.assessments();
        if !matches!(declaration, Declaration::NeedsInput { .. }) || !assessments.is_empty() {
            errors.extend(batch::assessment_errors(manifest, assessments));
        }
        if let Declaration::Final {
            edits, revision, ..
        } = declaration
        {
            errors.extend(batch::edit_errors(db, run, manifest, edits, assessments)?);
            if !matches!(revision, RevisionEvidence::Exact(sha) if sha.as_str() == manifest.source_sha)
            {
                errors.push(HandoffFieldError::new(
                    "/handoff/revision/sha",
                    format!(
                        "A selected-thread result reviews source SHA {}.",
                        manifest.source_sha
                    ),
                ));
            }
        }
    }
    let Declaration::Final {
        report: Some(report),
        edits,
        revision,
        ..
    } = declaration
    else {
        return Ok((errors, None));
    };
    let parent = match head {
        Some(head) => structured::parent::active(db, run, head)?,
        None => Vec::new(),
    };
    errors.extend(ledger::check(
        declaration,
        &parent,
        headings,
        manifest.is_none(),
    ));
    let reviewed = match revision {
        RevisionEvidence::Exact(sha) => Some(sha),
        RevisionEvidence::Unknown(_) => None,
    };
    let derived = ledger::derive(parent, report, edits, reviewed, &[]);
    Ok((errors, Some(structured::Projection::new(derived)?)))
}

/// The supplied details and those carried from the parent exceed the metadata budget.
pub(super) fn too_large() -> HandoffFieldError {
    HandoffFieldError::new(
        "/handoff/report/findings",
        format!(
            "With the details carried from the last accepted report, the findings exceed ReviewQ's {} KiB metadata budget. Shorten consequence and evidence text.",
            METADATA_BYTES / 1024
        ),
    )
}
