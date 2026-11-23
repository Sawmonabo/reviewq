use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Deserializer, json, value::RawValue};

use super::Failure;

const WHITESPACE: [char; 4] = [' ', '\t', '\r', '\n'];
const CONFIG: &str = "Could not parse the Claude profile config.";
const ENTRY: &str = "The existing Claude MCP entry was preserved.";

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
) -> Result<Option<Vec<u8>>, Failure> {
    let source = std::str::from_utf8(before.unwrap_or(b"{}\n")).map_err(|_| CONFIG)?;
    serde_json::from_str::<&RawValue>(source).map_err(|_| CONFIG)?;
    let checkout = checkout.to_str().ok_or("Expected a UTF-8 checkout path.")?;
    let path = ["projects", checkout, "mcpServers", "reviewq"];
    let mut object = skip_whitespace(source, 0);
    for (index, name) in path.iter().enumerate() {
        if source.as_bytes()[object] != b'{' {
            return match url {
                Some(_) => Err("Claude config contains an incompatible entry.".into()),
                None => Ok(None),
            };
        }
        let children = &path[index + 1..];
        match field(source, object, name)? {
            Field::Present { value, .. } if !children.is_empty() => object = value.start,
            Field::Present { value, remove } => {
                let existing = read_server(source, &value)?;
                if !owned.contains(&existing.url) || existing != Server::http(&existing.url, bearer)
                {
                    return Err(ENTRY.into());
                }
                return match url {
                    Some(url) if url == existing.url => Ok(None),
                    Some(url) => Ok(Some(replace(
                        source,
                        value,
                        &serde_json::to_vec(&Server::http(url, bearer))?,
                    ))),
                    None => Ok(Some(replace(source, remove, b""))),
                };
            }
            Field::Absent { insert, comma } => {
                let Some(url) = url else { return Ok(None) };
                let mut value = serde_json::to_value(Server::http(url, bearer))?;
                for &key in children.iter().rev() {
                    value = json!({key: value});
                }
                let mut member = Vec::new();
                if comma {
                    member.push(b',');
                }
                serde_json::to_writer(&mut member, name)?;
                member.push(b':');
                serde_json::to_writer(&mut member, &value)?;
                return Ok(Some(replace(source, insert..insert, &member)));
            }
        }
    }
    Ok(None)
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
