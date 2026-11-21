use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::{
    ContentReference, FileContentRef, FilePosition, HistoryBookmark, NativeHead, NativeHistoryPage,
    NativeNavigation, NativePlan, NativePosition, NavigationQuery, PromptGroup,
    PromptOverviewCheckpoint, PromptPosition, PromptPreview, SourceAnchor, VisibleKind,
    VisiblePart, VisiblePartKind,
};
use reviewq_domain::identity::NativeInputId;

use super::file::{self, Lines, MAX_RECORDS, READ_BUDGET};
use super::{locator::Source, records};

const MAX_CHECKPOINTS: usize = 4096;

pub(super) fn around(
    source: &Source,
    entry: &NativeInputId,
    anchor: Option<SourceAnchor>,
    limit: u16,
    cancel: &AtomicBool,
) -> Result<NativeHistoryPage, AppError> {
    if !(1..=100).contains(&limit) {
        return Err(AppError::InvalidRequest);
    }
    let current = head(source, None, cancel)?.anchor;
    let complete = current.file.high_water;
    let mut budget = READ_BUDGET;
    let at = if let Some(anchor) = anchor {
        if anchor.generation != current.generation {
            return Err(AppError::CursorExpired);
        }
        file::verify_position(&source.file, &anchor.file, complete)?;
        let start = anchor.file.offset;
        let end = file::next_line(&source.file, start, complete, &mut budget, cancel)?;
        if !record(source, start, end, 0, cancel).is_some_and(|record| record.id == *entry) {
            return Err(AppError::CursorExpired);
        }
        anchor.file
    } else {
        let mut offset = 0;
        let mut found = None;
        for _ in 0..MAX_RECORDS {
            if offset >= complete || budget == 0 {
                break;
            }
            let ending = file::next_line(&source.file, offset, complete, &mut budget, cancel)?;
            if record(source, offset, ending, 0, cancel).is_some_and(|record| record.id == *entry) {
                found = Some(file::file_position(&source.file, offset, complete)?);
                break;
            }
            offset = ending;
        }
        found.ok_or(if offset < complete {
            AppError::Unavailable
        } else {
            AppError::NotFound
        })?
    };
    let before = limit / 2;
    let older = if before == 0 {
        None
    } else {
        Some(file::page(
            source,
            NativePosition {
                file: Some(at.clone()),
                ..NativePosition::default()
            },
            before,
            cancel,
        )?)
    };
    let count = older.as_ref().map_or(0, |page| page.entries.len() as u16);
    let mut newer = file::page(
        source,
        NativePosition {
            file: Some(at),
            ascending: true,
            ..NativePosition::default()
        },
        limit - count,
        cancel,
    )?;
    if !newer.entries.iter().any(|record| record.id == *entry) {
        return Err(AppError::CursorExpired);
    }
    if let Some(mut older) = older {
        older.entries.append(&mut newer.entries);
        newer.entries = older.entries;
        newer.older = older.older;
    }
    Ok(newer)
}

pub(super) fn head(
    source: &Source,
    previous: Option<&SourceAnchor>,
    cancel: &AtomicBool,
) -> Result<NativeHead, AppError> {
    let length = source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len();
    let mut budget = READ_BUDGET;
    let complete = file::end_of_line(&source.file, length, &mut budget, cancel)?;
    let generation = file::generation(source)?;
    let reset = previous.is_some_and(|previous| {
        previous.generation != generation
            || file::verify_position(&source.file, &previous.file, complete).is_err()
    });
    let position = file::file_position(&source.file, complete, complete)?;
    Ok(NativeHead {
        path: reviewq_domain::session::AbsolutePath::from_canonical(source.path.clone())
            .map_err(|_| AppError::Forbidden)?,
        anchor: SourceAnchor {
            generation,
            file: position,
        },
        reset,
    })
}

pub(super) fn read(
    source: &Source,
    query: NavigationQuery,
    cancel: &AtomicBool,
) -> Result<NativeNavigation, AppError> {
    let anchor = head(source, None, cancel)?.anchor;
    let complete = anchor.file.high_water;
    match query {
        NavigationQuery::Overview { groups, start } => {
            let start = if let Some((start, generation)) = start {
                if generation != anchor.generation {
                    return Err(AppError::CursorExpired);
                }
                let point = start.bookmark.clone().into_file()?;
                file::verify_position(&source.file, &point, complete)?;
                start
            } else {
                PromptPosition {
                    next_ordinal: 1,
                    bookmark: HistoryBookmark::File(file::file_position(
                        &source.file,
                        0,
                        complete,
                    )?),
                }
            };
            overview(source, anchor.generation, groups, start, cancel)
        }
        NavigationQuery::Previews {
            start,
            first,
            limit,
            generation,
        } => {
            if generation != anchor.generation
                || !(1..=50).contains(&limit)
                || first < start.next_ordinal
            {
                return Err(AppError::CursorExpired);
            }
            let point = start.bookmark.clone().into_file()?;
            file::verify_position(&source.file, &point, complete)?;
            previews(source, generation, start, first, limit, cancel)
        }
    }
}

pub(super) fn extend(
    source: &Source,
    mut checkpoint: PromptOverviewCheckpoint,
    pinned_start: Option<FilePosition>,
    cancel: &AtomicBool,
) -> Result<PromptOverviewCheckpoint, AppError> {
    let observed = head(source, Some(&checkpoint.anchor), cancel)?;
    if observed.reset {
        return Err(AppError::CursorExpired);
    }
    let file = &source.file;
    let complete = observed.anchor.file.high_water;
    let end = if let Some(point) = pinned_start {
        if point.offset != checkpoint.start_offset {
            return Err(AppError::CursorExpired);
        }
        file::verify_position(file, &point, complete)?;
        point.high_water
    } else {
        complete
    };
    let from = checkpoint.anchor.file.high_water;
    if end < from {
        return Err(AppError::CursorExpired);
    }
    for group in &checkpoint.groups {
        let point = group.start.bookmark.clone().into_file()?;
        file::verify_position(file, &point, end)?;
    }
    if end == from {
        return Ok(checkpoint);
    }
    checkpoint.anchor.file = file::file_position(file, end, end)?;
    if let Some(next) = &mut checkpoint.next {
        // An incomplete scan has already published a full window. Widen its
        // continuation across an append without revisiting the completed prefix.
        let saved = next.bookmark.clone().into_file()?;
        file::verify_position(file, &saved, from)?;
        next.bookmark = HistoryBookmark::File(file::file_position(file, saved.offset, end)?);
        return Ok(checkpoint);
    }
    let resume = PromptPosition {
        next_ordinal: checkpoint.total.checked_add(1).ok_or(AppError::Internal)?,
        bookmark: HistoryBookmark::File(file::file_position(file, from, end)?),
    };
    let NativeNavigation::Overview {
        total,
        groups,
        next,
        ..
    } = overview(
        source,
        checkpoint.anchor.generation.clone(),
        32,
        resume,
        cancel,
    )?
    else {
        return Err(AppError::Internal);
    };
    checkpoint.groups.extend(groups);
    checkpoint.total = total;
    checkpoint.next = next;
    if checkpoint.groups.len() > 32 {
        checkpoint.groups = checkpoint
            .groups
            .chunks(2)
            .map(|pair| {
                let mut group = pair[0].clone();
                group.last = pair.last().expect("nonempty chunk").last;
                group
            })
            .collect();
    }
    let scanned = checkpoint
        .next
        .as_ref()
        .map(|point| point.bookmark.clone().into_file().map(|point| point.offset))
        .transpose()?
        .unwrap_or(end);
    for group in &mut checkpoint.groups {
        let saved = group.start.bookmark.clone().into_file()?;
        file::verify_position(file, &saved, scanned)?;
        group.start.bookmark =
            HistoryBookmark::File(file::file_position(file, saved.offset, scanned)?);
    }
    Ok(checkpoint)
}

fn overview(
    source: &Source,
    generation: String,
    groups: u16,
    start: PromptPosition,
    cancel: &AtomicBool,
) -> Result<NativeNavigation, AppError> {
    if !(1..=64).contains(&groups) {
        return Err(AppError::InvalidRequest);
    }
    let position = start.bookmark.into_file()?;
    let end = position.high_water;
    let mut offset = position.offset;
    let previous = start
        .next_ordinal
        .checked_sub(1)
        .ok_or(AppError::CursorExpired)?;
    let mut cursor = previous;
    let mut stride = 1_u64;
    let mut checkpoints = Vec::<(u64, u64)>::new();
    let mut budget = READ_BUDGET;
    let mut lines = Lines::new(&source.file, cancel);
    for _ in 0..MAX_RECORDS {
        if offset >= end || budget == 0 {
            break;
        }
        let next = match lines.next(offset, end, &mut budget) {
            Err(AppError::PayloadTooLarge) if offset > position.offset => break,
            result => result?,
        };
        if project(source, lines.input(offset, next), 0)
            .is_some_and(|record| record.kind == VisibleKind::User)
        {
            cursor += 1;
            if (cursor - 1 - previous) % stride == 0 {
                checkpoints.push((cursor, offset));
                if checkpoints.len() >= MAX_CHECKPOINTS {
                    stride *= 2;
                    checkpoints.retain(|(ordinal, _)| (ordinal - 1 - previous) % stride == 0);
                }
            }
        }
        offset = next;
    }
    let mut output = Vec::new();
    if cursor > previous {
        let width = (cursor - previous).div_ceil(u64::from(groups));
        let mut points = checkpoints.into_iter().peekable();
        let mut sample = points.next().ok_or(AppError::Internal)?;
        let mut first = start.next_ordinal;
        while first <= cursor {
            while points.peek().is_some_and(|(ordinal, _)| *ordinal <= first) {
                sample = points.next().ok_or(AppError::Internal)?;
            }
            let last = (first + width - 1).min(cursor);
            output.push(PromptGroup {
                first,
                last,
                start: PromptPosition {
                    next_ordinal: sample.0,
                    bookmark: HistoryBookmark::File(file::file_position(
                        &source.file,
                        sample.1,
                        offset,
                    )?),
                },
            });
            first = last + 1;
        }
    }
    let anchor = SourceAnchor {
        generation: generation.clone(),
        file: file::file_position(&source.file, end, end)?,
    };
    Ok(NativeNavigation::Overview {
        generation,
        source: Some(anchor),
        total: cursor,
        complete: offset == end,
        groups: output,
        next: (offset < end)
            .then(|| {
                Ok::<_, AppError>(PromptPosition {
                    next_ordinal: cursor + 1,
                    bookmark: HistoryBookmark::File(file::file_position(
                        &source.file,
                        offset,
                        end,
                    )?),
                })
            })
            .transpose()?,
    })
}

fn previews(
    source: &Source,
    generation: String,
    start: PromptPosition,
    first: u64,
    limit: u16,
    cancel: &AtomicBool,
) -> Result<NativeNavigation, AppError> {
    let position = start.bookmark.into_file()?;
    let end = position.high_water;
    let mut offset = position.offset;
    let mut ordinal = start.next_ordinal;
    let mut items = Vec::with_capacity(usize::from(limit));
    let mut budget = READ_BUDGET;
    let mut lines = Lines::new(&source.file, cancel);
    for _ in 0..MAX_RECORDS {
        if offset >= end || budget == 0 || items.len() == usize::from(limit) {
            break;
        }
        let next = lines.next(offset, end, &mut budget)?;
        if let Some(record) = project(
            source,
            lines.input(offset, next),
            if ordinal >= first { 644 } else { 0 },
        ) && record.kind == VisibleKind::User
        {
            if ordinal >= first {
                items.push(PromptPreview {
                    ordinal,
                    entry: record.id,
                    at_ms: record.at_ms,
                    text: file::readable_prefix(&record.text)?
                        .chars()
                        .take(160)
                        .collect(),
                    anchor: HistoryBookmark::File(file::file_position(&source.file, offset, end)?),
                    after: PromptPosition {
                        next_ordinal: ordinal + 1,
                        bookmark: HistoryBookmark::File(file::file_position(
                            &source.file,
                            next,
                            end,
                        )?),
                    },
                });
            }
            ordinal += 1;
        }
        offset = next;
    }
    let next = (offset < end)
        .then(|| {
            Ok::<_, AppError>(PromptPosition {
                next_ordinal: ordinal,
                bookmark: HistoryBookmark::File(file::file_position(&source.file, offset, end)?),
            })
        })
        .transpose()?;
    Ok(NativeNavigation::Previews {
        generation,
        complete: next.is_none(),
        items,
        next,
    })
}

pub(super) fn plan(source: &Source, cancel: &AtomicBool) -> Result<NativePlan, AppError> {
    let anchor = head(source, None, cancel)?.anchor;
    let complete = anchor.file.high_water;
    let mut offset = complete;
    let mut budget = READ_BUDGET;
    for _ in 0..MAX_RECORDS {
        if offset == 0 || budget == 0 {
            break;
        }
        let start = file::end_of_line(&source.file, offset - 1, &mut budget, cancel)?;
        if let Some(record) = record(source, start, offset, 2048, cancel)
            && let Some(part) = record
                .parts
                .into_iter()
                .rev()
                .find(|part| matches!(part.kind, VisiblePartKind::Plan))
        {
            let position = file::file_position(&source.file, start, complete)?;
            let text = file::readable_prefix(&part.body.picked)?.to_owned();
            let content = (part.body.bytes > text.len() as u64).then(|| {
                ContentReference::File(FileContentRef {
                    entry: record.id.clone(),
                    part: Some(part.index),
                    generation: anchor.generation.clone(),
                    position,
                })
            });
            return Ok(NativePlan {
                generation: anchor.generation,
                tasks: None,
                entry: Some((
                    record.id,
                    VisiblePart {
                        native_index: part.index,
                        kind: part.kind,
                        text_bytes: part.body.bytes,
                        text,
                        content,
                    },
                )),
            });
        }
        offset = start;
    }
    if offset != 0 {
        return Err(AppError::Unavailable);
    }
    Ok(NativePlan {
        generation: anchor.generation,
        entry: None,
        tasks: None,
    })
}

fn record(
    source: &Source,
    start: u64,
    end: u64,
    limit: usize,
    cancel: &AtomicBool,
) -> Option<records::Record> {
    project(source, file::input(&source.file, start, end, cancel), limit)
}

fn project(source: &Source, input: impl std::io::Read, limit: usize) -> Option<records::Record> {
    records::project(
        source.provider,
        input,
        &source.native_id,
        source.root_session.as_ref(),
        0,
        limit,
    )
}
