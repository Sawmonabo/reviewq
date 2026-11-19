use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_domain::session::{HostProvider, NativeBinding};
use serde::Deserialize;
use sha2::{Digest, Sha256};
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
    codex_call(binding, path, call_id, position, None).await
}

pub(crate) async fn codex_question_opening(
    binding: NativeBinding,
    path: PathBuf,
    call_id: String,
) -> Result<(), AppError> {
    codex_call(binding, path, call_id, None, Some("request_user_input")).await?;
    Ok(())
}

async fn codex_call(
    binding: NativeBinding,
    path: PathBuf,
    call_id: String,
    position: Option<u64>,
    name: Option<&'static str>,
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
                || !opening(
                    file::input(&source.file, position, end, &stopped),
                    &call_id,
                    name,
                )?
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
                if opening(
                    file::input(&source.file, start, end, &stopped),
                    &call_id,
                    name,
                )
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

pub(super) fn opening(
    input: impl Read,
    expected: &str,
    expected_tool: Option<&str>,
) -> Result<bool, AppError> {
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let (mut outer, mut kind, mut id, mut tool) = (None, None, None, None);
    object(&mut reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| outer = value),
        "payload" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| match name {
                "type" => string(reader, ID_BYTES).map(|value| kind = value),
                "call_id" => string(reader, ID_BYTES).map(|value| id = value),
                "name" if expected_tool.is_some() => {
                    string(reader, ID_BYTES).map(|value| tool = value)
                }
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
        && id.as_deref() == Some(expected)
        && expected_tool.is_none_or(|name| {
            kind.as_deref() == Some("function_call") && tool.as_deref() == Some(name)
        }))
}

pub(crate) async fn codex_question_answered(
    binding: NativeBinding,
    path: PathBuf,
    call_id: String,
    digest: [u8; 32],
) -> Result<bool, AppError> {
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
        let mut end = file::end_of_line(&source.file, length, &mut budget, &stopped)?;
        while end != 0 {
            let start = file::end_of_line(&source.file, end - 1, &mut budget, &stopped)?;
            if answer(
                file::input(&source.file, start, end, &stopped),
                &call_id,
                digest,
            )
            .unwrap_or(false)
            {
                return Ok(true);
            }
            end = start;
        }
        Ok(false)
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

fn answer(
    input: impl Read,
    expected_call: &str,
    expected_digest: [u8; 32],
) -> Result<bool, AppError> {
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let (mut outer, mut kind, mut call, mut output) = (None, None, None, None);
    object(&mut reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| outer = value),
        "payload" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| match name {
                "type" => string(reader, ID_BYTES).map(|value| kind = value),
                "call_id" => string(reader, ID_BYTES).map(|value| call = value),
                "output" if reader.peek().map_err(invalid)? == ValueType::String => {
                    string(reader, 48 * 1024).map(|value| output = value)
                }
                _ => reader.skip_value().map_err(invalid),
            })
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    if outer.as_deref() != Some("response_item")
        || kind.as_deref() != Some("function_call_output")
        || call.as_deref() != Some(expected_call)
    {
        return Ok(false);
    }
    #[derive(Deserialize)]
    struct ResultPayload {
        answers: BTreeMap<String, ResponseValues>,
    }
    #[derive(Deserialize)]
    struct ResponseValues {
        answers: Vec<String>,
    }
    let Some(result) = output
        .as_deref()
        .and_then(|output| serde_json::from_str::<ResultPayload>(output).ok())
    else {
        return Ok(false);
    };
    let normalized = result
        .answers
        .into_iter()
        .map(|(id, value)| (id, value.answers))
        .collect::<BTreeMap<_, _>>();
    let verified: [u8; 32] =
        Sha256::digest(serde_json::to_vec(&normalized).map_err(|_| AppError::Internal)?).into();
    Ok(verified == expected_digest)
}
