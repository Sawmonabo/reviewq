//! Exact main-loop Claude input readback under a pre-send file witness.

use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::{HistorySession, VisibleKind};
use reviewq_app::hosts::{ClaudeAnchor, MessageBody};
use reviewq_domain::identity::NativeInputId;
use reviewq_domain::session::HostProvider;

use super::{file, locator, records};

const OBSERVATION_BYTES: u64 = 4 * 1024 * 1024;
const MAX_NATIVE_PROMPT_BYTES: usize = 48_000;

pub(crate) fn claude_anchor(
    session: &HistorySession,
    cancelled: &AtomicBool,
) -> Result<ClaudeAnchor, AppError> {
    if session.parent.is_some() || session.binding.identity().provider() != HostProvider::Claude {
        return Err(AppError::Forbidden);
    }
    let source = locator::locate(session, cancelled)?.ok_or(AppError::NotFound)?;
    let length = source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len();
    let mut budget = OBSERVATION_BYTES;
    let complete = file::end_of_line(&source.file, length, &mut budget, cancelled)?;
    if complete == 0 || complete != length {
        return Err(AppError::Busy);
    }
    Ok(ClaudeAnchor {
        source_generation: file::generation(&source)?,
        file: file::file_position(&source.file, complete, complete)?,
    })
}

pub(crate) fn claude_confirm_prompt(
    session: &HistorySession,
    anchor: &ClaudeAnchor,
    entry: &NativeInputId,
    text: &MessageBody,
    cancelled: &AtomicBool,
) -> Result<(), AppError> {
    if session.parent.is_some() || session.binding.identity().provider() != HostProvider::Claude {
        return Err(AppError::Forbidden);
    }
    let source = locator::locate(session, cancelled)?.ok_or(AppError::NotFound)?;
    let length = source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len();
    if file::generation(&source)? != anchor.source_generation {
        return Err(AppError::Conflict);
    }
    let mut budget = OBSERVATION_BYTES;
    let complete = file::end_of_line(&source.file, length, &mut budget, cancelled)?;
    file::verify_position(&source.file, &anchor.file, complete).map_err(|_| AppError::Conflict)?;
    let mut offset = anchor.file.offset;
    while offset < complete {
        let next = file::next_line(&source.file, offset, complete, &mut budget, cancelled)?;
        if next <= offset {
            return Err(AppError::Unavailable);
        }
        if let Some(item) = records::project(
            HostProvider::Claude,
            file::input(&source.file, offset, next, cancelled),
            &source.native_id,
            None,
            0,
            MAX_NATIVE_PROMPT_BYTES + 1,
        ) && item.kind == VisibleKind::User
        {
            if item.id != *entry
                || item.bytes as usize != text.as_str().len()
                || item.text.as_slice() != text.as_str().as_bytes()
            {
                return Err(AppError::Conflict);
            }
            return Ok(());
        }
        offset = next;
    }
    Err(AppError::Busy)
}
