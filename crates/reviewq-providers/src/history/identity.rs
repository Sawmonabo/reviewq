use std::io::Read;
use std::path::PathBuf;

use reviewq_app::access::AppError;
use reviewq_domain::session::HostProvider;
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use super::reader::NameBudget;
use super::records::{ID_BYTES, invalid, object, string};

#[derive(Default)]
pub(super) struct IdentityRecord {
    pub session: Option<String>,
    pub cwd: Option<PathBuf>,
    pub parent: Option<String>,
    pub agent: Option<String>,
    pub sidechain: bool,
}

pub(super) fn read(provider: HostProvider, source: impl Read) -> Result<IdentityRecord, AppError> {
    let mut reader = JsonStreamReader::new(NameBudget::new(source));
    let mut proof = IdentityRecord::default();
    let mut kind = None;
    object(&mut reader, |name, reader| match (provider, name) {
        (HostProvider::Codex, "type") => string(reader, ID_BYTES).map(|value| kind = value),
        (HostProvider::Codex, "payload")
            if reader.peek().map_err(invalid)? == ValueType::Object =>
        {
            object(reader, |name, reader| match name {
                "id" => string(reader, ID_BYTES).map(|value| proof.session = value),
                "cwd" => string(reader, 4096).map(|value| proof.cwd = value.map(PathBuf::from)),
                "source" => {
                    proof.parent = codex_parent(reader)?;
                    Ok(())
                }
                _ => reader.skip_value().map_err(invalid),
            })
        }
        (HostProvider::Claude, "sessionId") => {
            string(reader, ID_BYTES).map(|value| proof.session = value)
        }
        (HostProvider::Claude, "agentId") => {
            string(reader, ID_BYTES).map(|value| proof.agent = value)
        }
        (HostProvider::Claude, "isSidechain") => reader
            .next_bool()
            .map(|value| proof.sidechain = value)
            .map_err(invalid),
        _ => reader.skip_value().map_err(invalid),
    })?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    if provider == HostProvider::Codex && kind.as_deref() != Some("session_meta") {
        return Err(AppError::Unavailable);
    }
    Ok(proof)
}

fn codex_parent<R: Read>(reader: &mut JsonStreamReader<R>) -> Result<Option<String>, AppError> {
    if reader.peek().map_err(invalid)? != ValueType::Object {
        reader.skip_value().map_err(invalid)?;
        return Ok(None);
    }
    let mut parent = None;
    object(reader, |name, reader| {
        if name != "subagent" || reader.peek().map_err(invalid)? != ValueType::Object {
            return reader.skip_value().map_err(invalid);
        }
        object(reader, |name, reader| {
            if name != "thread_spawn" || reader.peek().map_err(invalid)? != ValueType::Object {
                return reader.skip_value().map_err(invalid);
            }
            object(reader, |name, reader| match name {
                "parent_thread_id" => string(reader, ID_BYTES).map(|value| parent = value),
                _ => reader.skip_value().map_err(invalid),
            })
        })
    })?;
    Ok(parent)
}
