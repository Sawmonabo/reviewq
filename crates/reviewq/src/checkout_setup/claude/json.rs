use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Deserializer, value::RawValue};

use super::Failure;

const WHITESPACE: [char; 4] = [' ', '\t', '\r', '\n'];
const CONFIG: &str = "Could not parse the Claude profile config.";
const ENTRY: &str = "The existing Claude MCP entry was preserved.";
const ROLLBACK: &str =
    "Claude changed the ReviewQ MCP entry during setup. Its current value was preserved.";

pub(super) struct Patch {
    pub bytes: Vec<u8>,
    pub undo: Undo,
}

pub(super) struct Undo {
    checkout: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Server>,
}

enum Field {
    Present {
        value: Range<usize>,
        remove: Range<usize>,
    },
    Absent {
        insert: usize,
        comma: bool,
    },
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
    owned: &BTreeSet<String>,
    bearer: &str,
    url: Option<&str>,
) -> Result<Option<Patch>, Failure> {
    let source = source(before.unwrap_or(b"{}\n"))?;
    let path = entry_path(checkout)?;
    let after = url.map(|url| Server::http(url, bearer));
    let (bytes, before) = match locate(source, &path)? {
        Some((Field::Present { value, remove }, _)) => {
            let existing = read_server(source, &value)?;
            if !owned.contains(&existing.url) || existing != Server::http(&existing.url, bearer) {
                return Err(ENTRY.into());
            }
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
            before,
            after,
        },
    }))
}

impl Undo {
    pub(super) fn restore(&self, bytes: &[u8]) -> Result<Option<Vec<u8>>, Failure> {
        let source = source(bytes)?;
        let path = entry_path(&self.checkout)?;
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

fn source(bytes: &[u8]) -> Result<&str, Failure> {
    let source = std::str::from_utf8(bytes).map_err(|_| CONFIG)?;
    serde_json::from_str::<&RawValue>(source).map_err(|_| CONFIG)?;
    Ok(source)
}

fn entry_path(checkout: &Path) -> Result<[&str; 4], Failure> {
    let checkout = checkout.to_str().ok_or("Expected a UTF-8 checkout path.")?;
    Ok(["projects", checkout, "mcpServers", "reviewq"])
}

fn locate<'a>(
    source: &str,
    path: &'a [&'a str],
) -> Result<Option<(Field, &'a [&'a str])>, Failure> {
    let mut object = skip_whitespace(source, 0);
    for (index, name) in path.iter().enumerate() {
        if source.as_bytes()[object] != b'{' {
            return Ok(None);
        }
        match field(source, object, name)? {
            Field::Present { value, .. } if index + 1 < path.len() => object = value.start,
            entry => return Ok(Some((entry, &path[index..]))),
        }
    }
    Ok(None)
}

fn insert(
    source: &str,
    at: usize,
    comma: bool,
    names: &[&str],
    value: &[u8],
) -> Result<Vec<u8>, Failure> {
    let mut member = Vec::new();
    if comma {
        member.push(b',');
    }
    for (index, name) in names.iter().enumerate() {
        if index != 0 {
            member.push(b'{');
        }
        serde_json::to_writer(&mut member, name)?;
        member.push(b':');
    }
    member.extend_from_slice(value);
    member.extend(std::iter::repeat_n(b'}', names.len() - 1));
    Ok(replace(source, at..at, &member))
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

// Walk the members of a validated JSON object. Serde consumes each value without
// converting its numbers or strings, so a replacement retains the surrounding bytes.
fn field(source: &str, object: usize, wanted: &str) -> Result<Field, Failure> {
    let bytes = source.as_bytes();
    let mut previous_comma = None;
    let mut start = skip_whitespace(source, object + 1);
    if bytes[start] == b'}' {
        return Ok(Field::Absent {
            insert: object + 1,
            comma: false,
        });
    }
    let mut found = None;
    loop {
        let (name, end) = token::<String>(source, start)?;
        let colon = skip_whitespace(source, end);
        let value = skip_whitespace(source, colon + 1);
        let (_, end) = token::<&RawValue>(source, value)?;
        let separator = skip_whitespace(source, end);
        let has_next = bytes[separator] == b',';
        if name == wanted {
            if found.is_some() {
                return Err("The Claude config has more than one matching entry.".into());
            }
            let remove = if has_next {
                start..separator + 1
            } else {
                previous_comma.unwrap_or(start)..end
            };
            found = Some(Field::Present {
                value: value..end,
                remove,
            });
        }
        if !has_next {
            return Ok(found.unwrap_or(Field::Absent {
                insert: end,
                comma: true,
            }));
        }
        previous_comma = Some(separator);
        start = skip_whitespace(source, separator + 1);
    }
}

fn skip_whitespace(source: &str, offset: usize) -> usize {
    source.len() - source[offset..].trim_start_matches(WHITESPACE).len()
}

fn token<'a, T: Deserialize<'a>>(source: &'a str, offset: usize) -> Result<(T, usize), Failure> {
    let mut stream = Deserializer::from_str(&source[offset..]).into_iter::<T>();
    let value = stream.next().ok_or(CONFIG)?.map_err(|_| CONFIG)?;
    Ok((value, offset + stream.byte_offset()))
}

fn replace(source: &str, range: Range<usize>, value: &[u8]) -> Vec<u8> {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() - range.len() + value.len());
    output.extend_from_slice(&bytes[..range.start]);
    output.extend_from_slice(value);
    output.extend_from_slice(&bytes[range.end..]);
    output
}
