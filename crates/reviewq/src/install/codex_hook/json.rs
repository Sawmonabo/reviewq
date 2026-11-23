//! Preserve the existing JSON bytes and matcher positions when editing hooks.json.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::json_edit::{self, Field};

use super::{Failure, MATCHER, TIMEOUT_SECONDS};

const EVENT_PATH: &[&str] = &["hooks", "PreToolUse"];
const CHANGED: &str = "The Codex report hook has changed. ReviewQ preserved the current hooks.";
const INVALID: &str = "Expected a Codex hooks object with a PreToolUse array.";

type Change = Option<Option<Vec<u8>>>;

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Added {
    Document,
    HookMap,
    Event,
    Matcher,
}

#[derive(Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Group<'a> {
    matcher: &'a str,
    #[serde(borrow)]
    hooks: [Command<'a>; 1],
}

#[derive(Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Command<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    command: std::borrow::Cow<'a, str>,
    timeout: u32,
}

#[derive(Deserialize)]
struct Commands {
    hooks: Vec<CommandName>,
}

#[derive(Deserialize)]
struct CommandName {
    command: Option<String>,
}

enum Position {
    Absent {
        names: &'static [&'static str],
        at: usize,
        comma: bool,
    },
    Present {
        array: Range<usize>,
        remove_event: Range<usize>,
    },
}

impl Added {
    fn from_position(position: &Position) -> Self {
        match position {
            Position::Absent { names, .. } if *names == EVENT_PATH => Self::HookMap,
            Position::Absent { .. } => Self::Event,
            Position::Present { .. } => Self::Matcher,
        }
    }
}

pub(super) fn plan(bytes: Option<&[u8]>, command: &str) -> Result<Option<Added>, Failure> {
    let Some(bytes) = bytes else {
        return Ok(Some(Added::Document));
    };
    let source = json_edit::source(bytes)?;
    let position = position(source)?;
    if let Position::Present { array, .. } = &position {
        let items = items(source, array.start)?;
        if owned(source, &items, command)?.is_some() {
            return Ok(None);
        }
    }
    Ok(Some(Added::from_position(&position)))
}

pub(super) fn install(
    bytes: Option<&[u8]>,
    command: &str,
    added: Added,
    applied: bool,
) -> Result<Change, Failure> {
    let group = serde_json::to_vec(&group(command))?;
    let Some(bytes) = bytes else {
        return if added == Added::Document && !applied {
            Ok(Some(Some(document(&group))))
        } else {
            Err(CHANGED.into())
        };
    };
    let source = json_edit::source(bytes)?;
    let position = position(source)?;
    let next = match &position {
        Position::Absent { names, at, comma } => {
            if applied || Added::from_position(&position) != added {
                return Err(CHANGED.into());
            }
            json_edit::insert(source, *at, *comma, names, &array(&group))?
        }
        Position::Present { array, .. } => {
            let items = items(source, array.start)?;
            if owned(source, &items, command)?.is_some() {
                return Ok(None);
            }
            if applied || added != Added::Matcher {
                return Err(CHANGED.into());
            }
            let at = items.last().map_or(array.start + 1, |item| item.end);
            let mut insert = Vec::with_capacity(group.len() + 1);
            if !items.is_empty() {
                insert.push(b',');
            }
            insert.extend_from_slice(&group);
            json_edit::replace(source, at..at, &insert)
        }
    };
    Ok(Some(Some(next)))
}

pub(super) fn remove(bytes: Option<&[u8]>, command: &str, added: Added) -> Result<Change, Failure> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let source = json_edit::source(bytes)?;
    let Position::Present {
        array,
        remove_event,
    } = position(source)?
    else {
        return Ok(None);
    };
    let items = items(source, array.start)?;
    let Some(owned) = owned(source, &items, command)? else {
        return Ok(None);
    };
    if items.len() == 1 {
        let group = serde_json::to_vec(&group(command))?;
        if added == Added::Document && bytes == document(&group) {
            return Ok(Some(None));
        }
        if matches!(added, Added::Document | Added::HookMap)
            && let Some((Field::Present { value, remove }, _)) =
                json_edit::locate(source, &EVENT_PATH[..1])?
        {
            let canonical: serde_json::Value =
                serde_json::json!({"PreToolUse":[group_value(command)]});
            if serde_json::from_str::<serde_json::Value>(&source[value])
                .ok()
                .as_ref()
                == Some(&canonical)
            {
                return Ok(Some(Some(json_edit::replace(source, remove, b""))));
            }
        }
        if added != Added::Matcher {
            return Ok(Some(Some(json_edit::replace(source, remove_event, b""))));
        }
    }
    let item = items[owned].clone();
    let (range, replacement) = if owned < items.len() - 1 {
        // Codex trust keys include the matcher index. Keep later user hooks in
        // their current slots with an empty matcher group.
        (item, br#"{"hooks":[]}"#.as_slice())
    } else {
        let start = owned
            .checked_sub(1)
            .map_or(item.start, |prev| items[prev].end);
        (start..item.end, b"".as_slice())
    };
    Ok(Some(Some(json_edit::replace(source, range, replacement))))
}

fn position(source: &str) -> Result<Position, Failure> {
    match json_edit::locate(source, EVENT_PATH)? {
        Some((Field::Present { value, remove }, _)) if source.as_bytes()[value.start] == b'[' => {
            Ok(Position::Present {
                array: value,
                remove_event: remove,
            })
        }
        Some((Field::Absent { insert, comma }, names)) => Ok(Position::Absent {
            names,
            at: insert,
            comma,
        }),
        _ => Err(INVALID.into()),
    }
}

fn items(source: &str, array: usize) -> Result<Vec<Range<usize>>, Failure> {
    let mut entries = Vec::new();
    let mut start = json_edit::skip_whitespace(source, array + 1);
    while source.as_bytes()[start] != b']' {
        let (_, end) = json_edit::token::<&RawValue>(source, start)?;
        entries.push(start..end);
        let separator = json_edit::skip_whitespace(source, end);
        start = if source.as_bytes()[separator] == b',' {
            json_edit::skip_whitespace(source, separator + 1)
        } else {
            separator
        };
    }
    Ok(entries)
}

fn owned(source: &str, items: &[Range<usize>], command: &str) -> Result<Option<usize>, Failure> {
    let mut owned = None;
    let expected = group(command);
    for (index, range) in items.iter().enumerate() {
        let item = &source[range.clone()];
        if serde_json::from_str::<Group<'_>>(item).is_ok_and(|value| value == expected) {
            if owned.replace(index).is_some() {
                return Err(CHANGED.into());
            }
        } else if serde_json::from_str::<Commands>(item).is_ok_and(|group| {
            group
                .hooks
                .iter()
                .any(|handler| handler.command.as_deref() == Some(command))
        }) {
            return Err(CHANGED.into());
        }
    }
    Ok(owned)
}

fn group(command: &str) -> Group<'_> {
    Group {
        matcher: MATCHER,
        hooks: [Command {
            kind: "command",
            command: command.into(),
            timeout: TIMEOUT_SECONDS,
        }],
    }
}

fn group_value(command: &str) -> serde_json::Value {
    serde_json::json!(group(command))
}

fn document(group: &[u8]) -> Vec<u8> {
    [
        br#"{"hooks":{"PreToolUse":"#.as_slice(),
        &array(group),
        b"}}\n",
    ]
    .concat()
}

fn array(group: &[u8]) -> Vec<u8> {
    [b"[".as_slice(), group, b"]"].concat()
}
