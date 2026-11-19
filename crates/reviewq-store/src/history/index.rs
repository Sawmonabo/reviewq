use reviewq_app::access::{Actor, AppError};
use reviewq_app::history::{
    FilePosition, HistoryBookmark, HistorySession, PromptGroup, PromptOverviewCheckpoint,
    PromptPosition, SourceAnchor,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::history_session;
use crate::StoreError;

const MAX_GROUPS: usize = 32;

pub(super) fn read(
    db: &Connection,
    source: &HistorySession,
) -> Result<Option<PromptOverviewCheckpoint>, StoreError> {
    ensure_source(db, source)?;
    let key = source.target.session_key().as_str();
    let mut prepared = db.prepare_cached(
        "SELECT generation,pinned_end,source_witness,total,next_offset,next_witness
         FROM history_prompt_overviews WHERE session_key=?1",
    )?;
    let mut rows = prepared.query([key])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let pinned = integer(row.get(1)?)?;
    let total = integer(row.get(3)?)?;
    let anchor = SourceAnchor {
        generation: row.get(0)?,
        file: FilePosition {
            offset: pinned,
            high_water: pinned,
            witness: witness(row.get(2)?)?,
        },
    };
    let next = match row.get::<_, Option<i64>>(4)? {
        None => None,
        Some(offset) => Some(PromptPosition {
            next_ordinal: total.checked_add(1).ok_or(AppError::Internal)?,
            bookmark: HistoryBookmark::File(FilePosition {
                offset: integer(offset)?,
                high_water: pinned,
                witness: witness(row.get(5)?)?,
            }),
        }),
    };
    let mut query = db.prepare_cached(
        "SELECT first_ordinal,last_ordinal,start_ordinal,byte_offset,high_water,witness
           FROM history_prompt_groups WHERE session_key=?1 ORDER BY slot",
    )?;
    let mut rows = query.query([key])?;
    let mut groups = Vec::new();
    while let Some(row) = rows.next()? {
        groups.push(PromptGroup {
            first: integer(row.get(0)?)?,
            last: integer(row.get(1)?)?,
            start: PromptPosition {
                next_ordinal: integer(row.get(2)?)?,
                bookmark: HistoryBookmark::File(FilePosition {
                    offset: integer(row.get(3)?)?,
                    high_water: integer(row.get(4)?)?,
                    witness: witness(row.get(5)?)?,
                }),
            },
        });
    }
    Ok(Some(PromptOverviewCheckpoint {
        anchor,
        total,
        groups,
        next,
    }))
}

pub(super) fn remember(
    db: &Connection,
    source: &HistorySession,
    expected: Option<SourceAnchor>,
    checkpoint: PromptOverviewCheckpoint,
) -> Result<(), StoreError> {
    ensure_source(db, source)?;
    let key = source.target.session_key().as_str();
    let previous=db.query_row(
        "SELECT generation,pinned_end,source_witness FROM history_prompt_overviews WHERE session_key=?1",
        [key],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,Vec<u8>>(2)?)),
    ).optional()?;
    match (previous, expected) {
        (None, None) => {}
        (Some((generation, offset, mark)), Some(anchor))
            if generation == anchor.generation
                && integer(offset)? == anchor.file.offset
                && mark.as_slice() == anchor.file.witness => {}
        _ => return Ok(()),
    }
    if checkpoint.groups.len() > MAX_GROUPS
        || checkpoint.anchor.file.offset != checkpoint.anchor.file.high_water
    {
        return Err(AppError::InvalidRequest.into());
    }
    let next = checkpoint
        .next
        .map(|point| point.bookmark.into_file())
        .transpose()?;
    let bound = checkpoint.anchor.file.high_water;
    db.execute(
        "DELETE FROM history_prompt_overviews WHERE session_key=?1",
        [key],
    )?;
    db.execute(
        "INSERT INTO history_prompt_overviews VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            key,
            checkpoint.anchor.generation,
            sql(bound)?,
            checkpoint.anchor.file.witness.as_slice(),
            sql(checkpoint.total)?,
            next.as_ref().map(|p| sql(p.offset)).transpose()?,
            next.as_ref().map(|p| p.witness.as_slice())
        ],
    )?;
    let mut insert =
        db.prepare_cached("INSERT INTO history_prompt_groups VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
    for (slot, group) in checkpoint.groups.into_iter().enumerate() {
        let point = group.start.bookmark.into_file()?;
        if point.high_water > bound || group.last > checkpoint.total {
            return Err(AppError::InvalidRequest.into());
        }
        insert.execute(params![
            key,
            slot as i64,
            sql(group.first)?,
            sql(group.last)?,
            sql(group.start.next_ordinal)?,
            sql(point.offset)?,
            sql(point.high_water)?,
            point.witness.as_slice()
        ])?;
    }
    Ok(())
}

fn ensure_source(db: &Connection, source: &HistorySession) -> Result<(), StoreError> {
    let current = history_session(
        db,
        &Actor::local_operator(),
        source.target.run_id().clone(),
        source.target.session_key().clone(),
    )?;
    if current != *source {
        return Err(AppError::CursorExpired.into());
    }
    Ok(())
}

fn integer(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| AppError::Internal.into())
}
fn sql(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| AppError::InvalidRequest.into())
}
fn witness(bytes: Vec<u8>) -> Result<[u8; 32], StoreError> {
    bytes.try_into().map_err(|_| AppError::Internal.into())
}
