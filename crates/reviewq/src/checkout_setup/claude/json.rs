use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::json_edit::{Field, field, insert, locate, replace, source};

use super::Failure;

const ENTRY: &str = "The existing Claude MCP entry was preserved.";
const ROLLBACK: &str =
    "Claude changed the ReviewQ MCP entry during setup. Its current value was preserved.";

/// The entry's name, owned by the protocol so the MCP server's sentence for an unknown tool
/// names the same entry.
pub(super) const NAME: &str = reviewq_protocol::mcp::CLAUDE_MANAGEMENT_SERVER;
/// The name ReviewQ wrote before. Its own entry there is removed; another server's stays.
pub(super) const FORMER: &str = "reviewq";

pub(super) struct Patch {
    pub bytes: Vec<u8>,
    pub undo: Undo,
}

pub(super) struct Undo {
    checkout: PathBuf,
    name: &'static str,
    before: Option<Vec<u8>>,
    after: Option<Server>,
}

#[derive(Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Server {
    #[serde(rename = "type")]
    kind: String,
    url: String,
    headers: Headers,
}

#[derive(Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Headers {
    #[serde(rename = "Authorization")]
    authorization: String,
}

impl Server {
    fn http(url: &str, bearer: &str) -> Self {
        Self {
            kind: "http".to_owned(),
            url: url.to_owned(),
            headers: Headers {
                authorization: format!("Bearer {bearer}"),
            },
        }
    }
}

pub(super) fn edit(
    before: Option<&[u8]>,
    checkout: &Path,
    name: &'static str,
    owned: &BTreeSet<String>,
    bearer: &str,
    url: Option<&str>,
) -> Result<Option<Patch>, Failure> {
    let source = source(before.unwrap_or(b"{}\n"))?;
    let path = entry_path(checkout, name)?;
    let after = url.map(|url| Server::http(url, bearer));
    let (bytes, before) = match locate(source, &path)? {
        Some((Field::Present { value, remove }, _)) => {
            let ours = read_server(source, &value).ok().filter(|existing| {
                owned.contains(&existing.url) && *existing == Server::http(&existing.url, bearer)
            });
            let Some(existing) = ours else {
                return if name == FORMER {
                    Ok(None)
                } else {
                    Err(ENTRY.into())
                };
            };
            if after.as_ref() == Some(&existing) {
                return Ok(None);
            }
            let original = source.as_bytes()[value.clone()].to_vec();
            let bytes = match &after {
                Some(server) => replace(source, value, &serde_json::to_vec(server)?),
                None => replace(source, remove, b""),
            };
            (bytes, Some(original))
        }
        Some((Field::Absent { insert: at, comma }, names)) => {
            let Some(server) = &after else {
                return Ok(None);
            };
            (
                insert(source, at, comma, names, &serde_json::to_vec(server)?)?,
                None,
            )
        }
        None if after.is_none() => return Ok(None),
        None => return Err("Claude config contains an incompatible entry.".into()),
    };
    Ok(Some(Patch {
        bytes,
        undo: Undo {
            checkout: checkout.to_owned(),
            name,
            before,
            after,
        },
    }))
}

impl Undo {
    pub(super) fn restore(&self, bytes: &[u8]) -> Result<Option<Vec<u8>>, Failure> {
        let source = source(bytes)?;
        let path = entry_path(&self.checkout, self.name)?;
        match (locate(source, &path)?, &self.after) {
            (Some((Field::Absent { .. }, _)), _) if self.before.is_none() => Ok(None),
            (Some((Field::Present { value, .. }, _)), _)
                if self.before.as_deref() == Some(&source.as_bytes()[value.clone()]) =>
            {
                Ok(None)
            }
            (Some((Field::Present { value, remove }, _)), Some(expected))
                if read_server(source, &value).ok().as_ref() == Some(expected) =>
            {
                Ok(Some(match &self.before {
                    Some(original) => replace(source, value, original),
                    None => replace(source, remove, b""),
                }))
            }
            (Some((Field::Absent { insert: at, comma }, names)), None) if names.len() == 1 => {
                let original = self.before.as_deref().ok_or(ROLLBACK)?;
                insert(source, at, comma, names, original).map(Some)
            }
            _ => Err(ROLLBACK.into()),
        }
    }
}

fn entry_path<'a>(checkout: &'a Path, name: &'a str) -> Result<[&'a str; 4], Failure> {
    let checkout = checkout.to_str().ok_or("Expected a UTF-8 checkout path.")?;
    Ok(["projects", checkout, "mcpServers", name])
}

fn read_server(source: &str, value: &Range<usize>) -> Result<Server, Failure> {
    // Serde can also deserialize structs from sequences. Claude's entries are objects.
    if source.as_bytes()[value.start] != b'{' {
        return Err(ENTRY.into());
    }
    let Field::Present { value: headers, .. } = field(source, value.start, "headers")? else {
        return Err(ENTRY.into());
    };
    if source.as_bytes()[headers.start] != b'{' {
        return Err(ENTRY.into());
    }
    Ok(serde_json::from_str(&source[value.clone()]).map_err(|_| ENTRY)?)
}
