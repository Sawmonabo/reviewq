use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::FileContentRef;
use reviewq_app::history::document::{MAX_INDEX_BYTES, Query};
use reviewq_app::history::search::SearchPoint;
use reviewq_app::history::search::document::{Projection, project};
use reviewq_domain::session::SessionTarget;

use crate::history::documents::{self, Documents};
use crate::history::locator::Source;

use super::{MatchSource, Matches};

pub(super) fn scan(
    matches: &mut Matches<'_>,
    reader: &MatchSource<'_, '_>,
) -> Result<(), AppError> {
    if matches.body.bytes > MAX_INDEX_BYTES as u64 {
        matches.partial = true;
        return Ok(());
    }
    let offset = matches.point.window.unwrap_or_else(|| {
        if matches.reverse {
            matches.body.bytes - 1
        } else {
            0
        }
    });
    let projection = match read(
        reader.documents,
        reader.owner,
        reader.source,
        &matches.reference,
        offset,
        reader.cancel,
    ) {
        Ok(projection) => projection,
        Err(AppError::NotFound) => {
            matches.partial = true;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if matches
        .point
        .window
        .is_some_and(|at| at != projection.proof.byte_start)
    {
        return Err(AppError::CursorExpired);
    }
    matches.point.window = Some(projection.proof.byte_start);
    matches.document = Some(projection.proof);
    if let Some(text) = projection.text {
        matches.collect(&text, true);
    } else {
        matches.partial = true;
    }
    if matches.next.is_none()
        && let Some(window) = if matches.reverse {
            projection.previous
        } else {
            projection.next
        }
    {
        matches.next = Some(SearchPoint {
            window: Some(window),
            byte: if matches.reverse { u64::MAX } else { 0 },
            ..matches.point
        });
    }
    Ok(())
}

pub(super) fn read(
    documents: &Documents,
    owner: &SessionTarget,
    source: &Source,
    reference: &FileContentRef,
    offset: u64,
    cancel: &AtomicBool,
) -> Result<Projection, AppError> {
    let handle = documents.slot(owner.clone(), reference.clone())?;
    let mut cached = handle.try_lock_owned().map_err(|_| AppError::Busy)?;
    documents::read(
        &mut cached,
        source,
        reference,
        Query {
            offset,
            expected: None,
        },
        cancel,
    )
    .map(project)
}
