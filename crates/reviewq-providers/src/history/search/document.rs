use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::FileContentRef;
use reviewq_app::history::document::{Position, Query, WindowResult};
use reviewq_app::history::search::SearchPoint;
use reviewq_app::history::search::document::{Projection, occurrences, project};
use reviewq_app::reading::table::paged::MAX_INDEXED_BYTES;
use reviewq_domain::session::SessionTarget;

use crate::history::documents::{self, Documents};
use crate::history::locator::Source;

use super::{MatchSource, Matches};

pub(super) fn scan(
    matches: &mut Matches<'_>,
    reader: &MatchSource<'_, '_>,
) -> Result<bool, AppError> {
    if matches.body.bytes > MAX_INDEXED_BYTES {
        return Ok(false);
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
        true,
    ) {
        Ok(projection) => projection,
        Err(AppError::NotFound) => {
            matches.partial = true;
            return Ok(true);
        }
        Err(error) => return Err(error),
    };
    if projection.literal_body {
        return Ok(false);
    }
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
        let (hits, partial) = occurrences(
            &text,
            matches.query,
            matches.boundary,
            matches.reverse,
            matches.available,
        );
        matches.partial |= partial;
        for span in hits {
            let point = SearchPoint {
                byte: span.start.byte as u64,
                ..matches.point
            };
            let witness = &text[span.start.byte..span.end.byte];
            matches
                .hits
                .push_back(matches.hit(&text, span, point, witness));
        }
        if !matches.reverse
            && matches.hits.len() == matches.available
            && let Some(hit) = matches.hits.back()
        {
            matches.next = Some(SearchPoint {
                byte: hit.point.byte + 1,
                ..hit.point
            });
        }
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
    Ok(true)
}

pub(super) fn read(
    documents: &Documents,
    owner: &SessionTarget,
    source: &Source,
    reference: &FileContentRef,
    offset: u64,
    cancel: &AtomicBool,
    include_context: bool,
) -> Result<Projection, AppError> {
    let claim = documents.try_claim(owner.clone(), reference.clone())?;
    let mut cached = claim
        .handle
        .clone()
        .try_lock_owned()
        .map_err(|_| AppError::Busy)?;
    let window = documents::read(
        &mut cached,
        source,
        reference,
        Query {
            position: Position::AtByte(offset),
            expected: None,
        },
        cancel,
    )?;
    if include_context
        && matches!(
            window.result,
            WindowResult::Source {
                canonical: true,
                ..
            }
        )
    {
        return documents::read(
            &mut cached,
            source,
            reference,
            Query {
                position: Position::Canonical,
                expected: Some(window.hash),
            },
            cancel,
        )
        .map(project);
    }
    Ok(project(window))
}
