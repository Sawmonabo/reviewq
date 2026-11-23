//! Byte-preserving edits for validated user JSON files.

use std::ops::Range;

use serde::Deserialize;
use serde_json::{Deserializer, value::RawValue};

type Failure = Box<dyn std::error::Error + Send + Sync>;

const WHITESPACE: [char; 4] = [' ', '\t', '\r', '\n'];
const CONFIG: &str = "Could not parse the profile JSON.";

pub(crate) enum Field {
    Present {
        value: Range<usize>,
        remove: Range<usize>,
    },
    Absent {
        insert: usize,
        comma: bool,
    },
}

pub(crate) fn source(bytes: &[u8]) -> Result<&str, Failure> {
    let source = std::str::from_utf8(bytes).map_err(|_| CONFIG)?;
    serde_json::from_str::<&RawValue>(source).map_err(|_| CONFIG)?;
    Ok(source)
}

pub(crate) fn locate<'a>(
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

pub(crate) fn insert(
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

// Walk the members of a validated JSON object. Serde consumes each value without
// converting its numbers or strings, so a replacement retains the surrounding bytes.
pub(crate) fn field(source: &str, object: usize, wanted: &str) -> Result<Field, Failure> {
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
                return Err("The profile JSON has more than one matching entry.".into());
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

pub(crate) fn skip_whitespace(source: &str, offset: usize) -> usize {
    source.len() - source[offset..].trim_start_matches(WHITESPACE).len()
}

pub(crate) fn token<'a, T: Deserialize<'a>>(
    source: &'a str,
    offset: usize,
) -> Result<(T, usize), Failure> {
    let mut stream = Deserializer::from_str(&source[offset..]).into_iter::<T>();
    let value = stream.next().ok_or(CONFIG)?.map_err(|_| CONFIG)?;
    Ok((value, offset + stream.byte_offset()))
}

pub(crate) fn replace(source: &str, range: Range<usize>, value: &[u8]) -> Vec<u8> {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() - range.len() + value.len());
    output.extend_from_slice(&bytes[..range.start]);
    output.extend_from_slice(value);
    output.extend_from_slice(&bytes[range.end..]);
    output
}
