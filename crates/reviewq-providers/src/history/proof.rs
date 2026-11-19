use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_domain::session::{HostProvider, NativeBinding};
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use super::reader::NameBudget;
use super::records::{ID_BYTES, invalid, object, string};
use super::{Cancel, file, locator};

const SEARCH_BYTES: u64 = 16 * 1024 * 1024;

/// The original tool invocation's ordered address in the verified rollout.
pub(crate) async fn codex_opening(
    binding: NativeBinding,
    path: PathBuf,
    call_id: String,
    position: Option<u64>,
) -> Result<(String, u64), AppError> {
    if binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::Forbidden);
    }
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let stopped = cancel.0.clone();
    tokio::task::spawn_blocking(move || {
        let source =
            locator::verify(&binding, None, &path, &stopped)?.ok_or(AppError::Unavailable)?;
        let length = source
            .file
            .metadata()
            .map_err(|_| AppError::Unavailable)?
            .len();
        let mut budget = SEARCH_BYTES;
        let offset = if let Some(position) = position {
            if position >= length || !file::boundary(&source.file, position)? {
                return Err(AppError::Conflict);
            }
            let end = file::next_line(&source.file, position, length, &mut budget, &stopped)?;
            if !file::boundary(&source.file, end)?
                || !opening(file::input(&source.file, position, end, &stopped), &call_id)?
            {
                return Err(AppError::Conflict);
            }
            position
        } else {
            let mut end = file::end_of_line(&source.file, length, &mut budget, &stopped)?;
            loop {
                if end == 0 {
                    return Err(AppError::Unavailable);
                }
                let start = file::end_of_line(&source.file, end - 1, &mut budget, &stopped)?;
                if opening(file::input(&source.file, start, end, &stopped), &call_id)
                    .unwrap_or(false)
                {
                    break start;
                }
                end = start;
            }
        };
        Ok((file::generation(&source)?, offset))
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

fn opening(input: impl Read, expected: &str) -> Result<bool, AppError> {
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let (mut outer, mut kind, mut id) = (None, None, None);
    object(&mut reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| outer = value),
        "payload" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| match name {
                "type" => string(reader, ID_BYTES).map(|value| kind = value),
                "call_id" => string(reader, ID_BYTES).map(|value| id = value),
                _ => reader.skip_value().map_err(invalid),
            })
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    Ok(outer.as_deref() == Some("response_item")
        && matches!(
            kind.as_deref(),
            Some("function_call" | "custom_tool_call" | "local_shell_call")
        )
        && id.as_deref() == Some(expected))
}
