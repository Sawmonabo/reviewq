//! Find the unique exact passage in a published report.

use reviewq_app::access::AppError;
use reviewq_app::reports::QuoteResolution;
use rusqlite::Connection;

use super::{Version, raw, read_count};
use crate::StoreError;

pub(super) fn resolve(
    connection: &Connection,
    version: &Version,
    text: &str,
) -> Result<QuoteResolution, StoreError> {
    let stats = &version.summary.stats;
    stats.require_markdown_budget()?;
    if text.trim().is_empty() {
        return Err(AppError::InvalidRequest.into());
    }

    let mut first = None;
    let mut bytes = 0;
    let mut scalars = 0;
    let mut carry = String::new();
    let mut carried_scalars = 0;
    let mut combined = String::new();
    let mut statement = connection.prepare_cached(
        "SELECT byte_start, scalar_start, raw
         FROM artifact_chunks WHERE artifact_id=?1 ORDER BY ordinal",
    )?;
    let mut rows = statement.query([&version.artifact_id])?;
    while let Some(row) = rows.next()? {
        if read_count(row, 0)? != bytes || read_count(row, 1)? != scalars {
            return Err(AppError::Internal.into());
        }
        let chunk = raw(row, 2)?;
        bytes += chunk.len() as u64;
        if bytes > stats.byte_length {
            return Err(AppError::Internal.into());
        }
        let chunk = str::from_utf8(chunk).map_err(|_| AppError::Internal)?;
        let window = if carry.is_empty() {
            chunk
        } else {
            combined.clear();
            combined.push_str(&carry);
            combined.push_str(chunk);
            combined.as_str()
        };
        if let Some(index) = window.find(text) {
            let next_scalar = index + window[index..].ceil_char_boundary(1);
            if first.is_some() || window[next_scalar..].contains(text) {
                return Ok(QuoteResolution::Ambiguous);
            }
            first = Some(scalars - carried_scalars + window[..index].chars().count() as u64);
        }
        scalars += chunk.chars().count() as u64;
        if bytes < stats.byte_length {
            // Keep only enough complete UTF-8 scalars for a later match that
            // crosses this chunk boundary. It cannot duplicate an earlier hit.
            let keep_from = window.ceil_char_boundary(window.len().saturating_sub(text.len() - 1));
            carry.clear();
            carry.push_str(&window[keep_from..]);
            carried_scalars = carry.chars().count() as u64;
        }
    }
    if bytes != stats.byte_length || scalars != stats.total_code_points {
        return Err(AppError::Internal.into());
    }
    Ok(match first {
        Some(offset) => QuoteResolution::Unique {
            offset,
            length: text.chars().count() as u64,
        },
        None => QuoteResolution::Missing,
    })
}
