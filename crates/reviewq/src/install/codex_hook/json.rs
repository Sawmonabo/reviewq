//! Preserve the existing JSON bytes and matcher positions when editing one event's array in
//! hooks.json.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::json_edit::{self, Field};

use super::{Event, Failure, TIMEOUT_SECONDS};

const CHANGED: &str = "The Codex hook has changed. ReviewQ preserved the current hooks.";
const INVALID: &str = "Expected a Codex hooks object whose Stop and SessionStart are arrays.";
// Codex trust keys include the matcher index, so a removed group that has a later one keeps
// its slot as this.
const EMPTY_GROUP: &[u8] = br#"{"hooks":[]}"#;

pub(super) type Change = Option<Option<Vec<u8>>>;

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Added {
    Document,
    HookMap,
    Event,
    Matcher,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Group<'a> {
    #[serde(borrow, default, skip_serializing_if = "Option::is_none")]
    matcher: Option<&'a str>,
    #[serde(borrow)]
    hooks: [Command<'a>; 1],
}

#[derive(Deserialize, Serialize)]
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

impl Commands {
    /// How many of the group's hooks run a command starting with `prefix`.
    fn running(&self, prefix: &str) -> usize {
        let commands = self.hooks.iter().filter_map(|hook| hook.command.as_deref());
        commands
            .filter(|command| command.starts_with(prefix))
            .count()
    }
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
    fn from_position(position: &Position, event: Event) -> Self {
        match position {
            Position::Absent { names, .. } if *names == event.path() => Self::HookMap,
            Position::Absent { .. } => Self::Event,
            Position::Present { .. } => Self::Matcher,
        }
    }
}

/// What installing `event`'s entry would add to `bytes`, or `None` when it is already there.
pub(super) fn plan(
    bytes: Option<&[u8]>,
    event: Event,
    command: &str,
) -> Result<Option<Added>, Failure> {
    let Some(bytes) = bytes else {
        return Ok(Some(Added::Document));
    };
    let source = json_edit::source(bytes)?;
    let position = position(source, event)?;
    if let Position::Present { array, .. } = &position {
        let items = items(source, array.start)?;
        if owned(source, &items, event, command)?.is_some() {
            return Ok(None);
        }
    }
    Ok(Some(Added::from_position(&position, event)))
}

pub(super) fn install(
    bytes: Option<&[u8]>,
    event: Event,
    command: &str,
    added: Added,
    applied: bool,
) -> Result<Change, Failure> {
    let group = serde_json::to_vec(&group(event, command))?;
    let Some(bytes) = bytes else {
        return if added == Added::Document && !applied {
            Ok(Some(Some(document(event, &group))))
        } else {
            Err(CHANGED.into())
        };
    };
    let source = json_edit::source(bytes)?;
    let position = position(source, event)?;
    let next = match &position {
        Position::Absent { names, at, comma } => {
            if applied || Added::from_position(&position, event) != added {
                return Err(CHANGED.into());
            }
            json_edit::insert(source, *at, *comma, names, &array(&group))?
        }
        Position::Present { array, .. } => {
            let items = items(source, array.start)?;
            if owned(source, &items, event, command)?.is_some() {
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

pub(super) fn remove(
    bytes: Option<&[u8]>,
    event: Event,
    command: &str,
    added: Added,
) -> Result<Change, Failure> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let source = json_edit::source(bytes)?;
    let Position::Present {
        array,
        remove_event,
    } = position(source, event)?
    else {
        return Ok(None);
    };
    let items = items(source, array.start)?;
    let Some(owned) = owned(source, &items, event, command)? else {
        return Ok(None);
    };
    if items.len() == 1 {
        let group = serde_json::to_vec(&group(event, command))?;
        if added == Added::Document && bytes == document(event, &group) {
            return Ok(Some(None));
        }
        if matches!(added, Added::Document | Added::HookMap)
            && let Some((Field::Present { value, remove }, _)) =
                json_edit::locate(source, &event.path()[..1])?
        {
            let canonical: serde_json::Value =
                serde_json::json!({event.name(): [group_value(event, command)]});
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
    let (range, replacement) = cut(&items, owned, owned < items.len() - 1);
    Ok(Some(Some(json_edit::replace(source, range, replacement))))
}

/// The edit that takes `items[index]` out of its array: a group with a `later` one leaves its
/// slot empty, and the last one goes with the comma before it.
fn cut(items: &[Range<usize>], index: usize, later: bool) -> (Range<usize>, &'static [u8]) {
    let item = items[index].clone();
    if later {
        return (item, EMPTY_GROUP);
    }
    let start = index
        .checked_sub(1)
        .map_or(item.start, |previous| items[previous].end);
    (start..item.end, b"")
}

/// Removes every group whose hooks all run a command starting with `prefix`, under whichever
/// event it sits, keeping later groups in their slots and any group that runs `keep`. An
/// array this empties stays: nothing says who made it. Returns the change and whether a group
/// that runs such a command beside other hooks was left as it is.
pub(super) fn sweep(
    bytes: Option<&[u8]>,
    prefix: &str,
    keep: Option<&str>,
) -> Result<(Change, bool), Failure> {
    let Some(bytes) = bytes else {
        return Ok((None, false));
    };
    let source = json_edit::source(bytes)?;
    let Some((Field::Present { value, .. }, _)) = json_edit::locate(source, &["hooks"])? else {
        return Ok((None, false));
    };
    let Ok(events) =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&source[value])
    else {
        return Ok((None, false));
    };
    let mut cuts: Vec<(Range<usize>, &[u8])> = Vec::new();
    let mut mixed = false;
    for name in events
        .iter()
        .filter_map(|(name, value)| value.is_array().then_some(name))
    {
        let Some((Field::Present { value, .. }, _)) =
            json_edit::locate(source, &["hooks", name.as_str()])?
        else {
            continue;
        };
        let items = items(source, value.start)?;
        let mut later = false;
        for index in (0..items.len()).rev() {
            let (ours, all) = serde_json::from_str::<Commands>(&source[items[index].clone()])
                .ok()
                .filter(|group| keep.is_none_or(|keep| group.running(keep) == 0))
                .map_or((0, 0), |group| (group.running(prefix), group.hooks.len()));
            if ours != 0 && ours == all {
                cuts.push(cut(&items, index, later));
            } else {
                mixed |= ours != 0;
                later = true;
            }
        }
    }
    if cuts.is_empty() {
        return Ok((None, mixed));
    }
    cuts.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut next = bytes.to_vec();
    for (range, replacement) in cuts {
        next.splice(range, replacement.iter().copied());
    }
    Ok((Some(Some(next)), mixed))
}

fn position(source: &str, event: Event) -> Result<Position, Failure> {
    match json_edit::locate(source, event.path())? {
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

fn owned(
    source: &str,
    items: &[Range<usize>],
    event: Event,
    command: &str,
) -> Result<Option<usize>, Failure> {
    let mut owned = None;
    let expected = group_value(event, command);
    for (index, range) in items.iter().enumerate() {
        let item = &source[range.clone()];
        if serde_json::from_str::<serde_json::Value>(item)
            .ok()
            .as_ref()
            == Some(&expected)
            && serde_json::from_str::<Group<'_>>(item).is_ok()
        {
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

fn group(event: Event, command: &str) -> Group<'_> {
    Group {
        matcher: event.matcher(),
        hooks: [Command {
            kind: "command",
            command: command.into(),
            timeout: TIMEOUT_SECONDS,
        }],
    }
}

fn group_value(event: Event, command: &str) -> serde_json::Value {
    serde_json::json!(group(event, command))
}

fn document(event: Event, group: &[u8]) -> Vec<u8> {
    [
        br#"{"hooks":{""#.as_slice(),
        event.name().as_bytes(),
        br#"":"#,
        &array(group),
        b"}}\n",
    ]
    .concat()
}

fn array(group: &[u8]) -> Vec<u8> {
    [b"[".as_slice(), group, b"]"].concat()
}
