//! ReviewQ's `[mcp_servers.reviewq]` table in a Codex config: writing it, changing its URL
//! and removing it, each only where its marker, a recorded URL and the bearer prove it
//! is ReviewQ's.

use std::collections::BTreeSet;

use toml_edit::{DocumentMut, Item, Table, value};

use super::MARKER;
use super::files::Existing;

/// The Codex config `before` with ReviewQ's table at `url`, or without it when `url` is
/// `None`, and `None` then when it has no such table. `owned` holds the URLs ReviewQ wrote.
/// The file keeps its own line ends and whether it ends with one.
pub(super) fn edit(
    before: Option<&Existing>,
    owned: &BTreeSet<String>,
    bearer: &str,
    url: Option<&str>,
) -> Result<Option<Vec<u8>>, Refusal> {
    let text = before
        .map_or(Ok(""), |file| std::str::from_utf8(&file.bytes))
        .map_err(|_| Refusal::Text)?;
    let mut document: DocumentMut = text.parse().map_err(|error: toml_edit::TomlError| {
        let at = error.span().map_or(0, |span| span.start);
        Refusal::Line(text[..at].matches('\n').count() + 1)
    })?;
    let configuration = document.as_table_mut();
    if url.is_none() && configuration.get("mcp_servers").is_none() {
        return Ok(None);
    }
    // ReviewQ writes its server only as a table with its own header, which an inline
    // `mcp_servers = { … }` cannot hold, and never writes a bearer it could not remove later.
    if let Some(inline) = configuration
        .get("mcp_servers")
        .and_then(Item::as_inline_table)
    {
        return match url {
            Some(_) => Err(Refusal::Inline),
            None if inline.contains_key("reviewq") => Err(Refusal::Entry),
            None => Ok(None),
        };
    }
    // An `mcp_servers` table setup adds is implicit, with no header of its own, so removing
    // ReviewQ's table removes it too and leaves no `[mcp_servers]` line behind.
    let servers = configuration
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut servers = Table::new();
            servers.set_implicit(true);
            Item::Table(servers)
        })
        .as_table_mut()
        .ok_or(Refusal::Servers)?;
    let authorization = format!("Bearer {bearer}");
    if let Some(existing) = servers.get("reviewq") {
        let table = existing.as_table().ok_or(Refusal::Entry)?;
        let old_url = table.get("url").and_then(Item::as_str);
        if !ours(table, owned, &authorization) {
            return Err(Refusal::Entry);
        }
        if old_url == url {
            return Ok(before.map(|file| file.bytes.clone()));
        }
    } else if url.is_none() {
        return Ok(None);
    }
    let Some(url) = url else {
        let removed = servers.remove("reviewq");
        if configuration
            .get("mcp_servers")
            .and_then(Item::as_table)
            .is_some_and(|table| table.is_implicit() && table.is_empty())
        {
            configuration.remove("mcp_servers");
        }
        if let Some(removed) = removed.as_ref().and_then(Item::as_table) {
            reattach(&mut document, removed);
        }
        return Ok(Some(styled(text, document.to_string(), true).into_bytes()));
    };
    // ReviewQ's table keeps its place and the lines above its header; only its URL changes.
    if let Some(table) = servers.get_mut("reviewq").and_then(Item::as_table_mut) {
        table.insert("url", value(url));
    } else {
        let mut headers = Table::new();
        headers.insert("Authorization", value(authorization.as_str()));
        let mut server = Table::new();
        server.decor_mut().set_prefix(format!("\n{MARKER}\n"));
        server.insert("url", value(url));
        server.insert("http_headers", Item::Table(headers));
        servers.insert("reviewq", Item::Table(server));
    }
    let written = styled(text, document.to_string(), false);
    // What holds the bearer must read back as ReviewQ's own table, so it can be removed.
    let proven = written.parse::<DocumentMut>().ok().and_then(|document| {
        let servers = document.get("mcp_servers")?.as_table()?;
        let table = servers.get("reviewq")?.as_table()?;
        Some(ours(
            table,
            &BTreeSet::from([url.to_owned()]),
            &authorization,
        ))
    });
    match proven {
        Some(true) => Ok(Some(written.into_bytes())),
        _ => Err(Refusal::Unprovable),
    }
}

/// Whether `table` is the one ReviewQ wrote: its marker above the header, a URL in `owned`,
/// and only the header with this key's bearer.
fn ours(table: &Table, owned: &BTreeSet<String>, authorization: &str) -> bool {
    let marker = table
        .decor()
        .prefix()
        .and_then(|text| text.as_str())
        .is_some_and(|text| text.lines().any(|line| line.trim() == MARKER));
    let url = table.get("url").and_then(Item::as_str);
    let headers = table.get("http_headers").and_then(Item::as_table);
    marker
        && table.len() == 2
        && url.is_some_and(|value| owned.contains(value))
        && headers.is_some_and(|headers| {
            headers.len() == 1
                && headers.get("Authorization").and_then(Item::as_str) == Some(authorization)
        })
}

/// `rendered`, which the TOML editor writes with LF line ends and a final one, in the style of
/// `original`: CRLF line ends when each of its line ends is one, and no final line end when
/// it has none. After a `removal`, blank lines it now starts with that `original` did not,
/// left where ReviewQ's table was at the top, go too.
fn styled(original: &str, rendered: String, removal: bool) -> String {
    let mut text = rendered;
    if removal && !original.starts_with(['\n', '\r']) {
        let start = text.len() - text.trim_start().len();
        let blank = text[..start].rfind('\n').map_or(0, |end| end + 1);
        text.drain(..blank);
    }
    let crlf = original.contains("\r\n")
        && original.matches('\n').count() == original.matches("\r\n").count();
    if crlf {
        let mut converted = String::with_capacity(text.len() + text.len() / 16);
        let mut previous = '\0';
        for character in text.chars() {
            if character == '\n' && previous != '\r' {
                converted.push('\r');
            }
            converted.push(character);
            previous = character;
        }
        text = converted;
    }
    if !original.is_empty() && !original.ends_with('\n') {
        let end = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .map(str::len);
        if let Some(end) = end {
            text.truncate(end);
        }
    }
    text
}

/// Puts the lines above the header of `removed`, ReviewQ's table, that are not ReviewQ's back
/// where they read the same: before the next header in the file, or at its end. Such as a
/// comment the user wrote there, which the parser gives to the header after it.
fn reattach(document: &mut DocumentMut, removed: &Table) {
    let prefix = removed.decor().prefix().and_then(|text| text.as_str());
    let mut lines = prefix
        .unwrap_or("")
        .split_inclusive('\n')
        .collect::<Vec<_>>();
    // ReviewQ's marker line, any copy of it, and the blank line setup writes before it.
    if let Some(marker) = lines.iter().position(|line| line.trim() == MARKER) {
        if marker > 0 && lines[marker - 1].trim().is_empty() {
            lines.remove(marker - 1);
        }
        lines.retain(|line| line.trim() != MARKER);
    }
    let kept = lines.concat();
    if kept.is_empty() {
        return;
    }
    // A parsed file gives each header its place, in the order the file shows them.
    let next = removed
        .position()
        .and_then(|after| next_header(document.as_table(), after));
    match next.and_then(|at| header_at(document.as_table_mut(), at)) {
        Some(table) => {
            let rest = table.decor().prefix().and_then(|text| text.as_str());
            let prefix = format!("{kept}{}", rest.unwrap_or(""));
            table.decor_mut().set_prefix(prefix);
        }
        None => {
            let trailing = format!("{kept}{}", document.trailing().as_str().unwrap_or(""));
            document.set_trailing(trailing);
        }
    }
}

/// The first place after `after` of a header in `table` or a table inside it.
fn next_header(table: &Table, after: isize) -> Option<isize> {
    let inside = table.iter().filter_map(|(_, item)| match item {
        Item::Table(child) => next_header(child, after),
        Item::ArrayOfTables(array) => array
            .iter()
            .filter_map(|child| next_header(child, after))
            .min(),
        _ => None,
    });
    table
        .position()
        .filter(|at| *at > after)
        .into_iter()
        .chain(inside)
        .min()
}

/// The table whose header is at place `at`: `table` or a table inside it.
fn header_at(table: &mut Table, at: isize) -> Option<&mut Table> {
    if table.position() == Some(at) {
        return Some(table);
    }
    table.iter_mut().find_map(|(_, item)| match item {
        Item::Table(child) => header_at(child, at),
        Item::ArrayOfTables(array) => array.iter_mut().find_map(|child| header_at(child, at)),
        _ => None,
    })
}

/// Why setup leaves a Codex config as it is.
pub(super) enum Refusal {
    Text,
    Line(usize),
    Servers,
    Inline,
    /// ReviewQ's table, once written, would not read back as its own.
    Unprovable,
    Entry,
}

impl Refusal {
    /// The cause, as a clause about the file, and the user's step. `ours` says whether
    /// ReviewQ wrote the file's [mcp_servers.reviewq] table before.
    pub(super) fn words(&self, ours: bool) -> (String, &'static str) {
        match self {
            Self::Text => ("it is not UTF-8 text".into(), "save it as UTF-8 text"),
            Self::Line(line) => (
                format!("line {line} there is not valid TOML"),
                "correct that line",
            ),
            Self::Servers => (
                "its `mcp_servers` entry is not a table".into(),
                "make `mcp_servers` there a table",
            ),
            Self::Inline => (
                "its `mcp_servers` is written inline, as `mcp_servers = { … }`, which cannot hold a table of ReviewQ's own".into(),
                "replace that line with an [mcp_servers.<name>] table for each server it lists, or delete it when it lists none",
            ),
            Self::Unprovable => (
                "the [mcp_servers.reviewq] table ReviewQ would write there would not read back as its own".into(),
                "write each server there as an [mcp_servers.<name>] table",
            ),
            Self::Entry if ours => (
                "its [mcp_servers.reviewq] table was changed by hand".into(),
                "remove that table there",
            ),
            Self::Entry => (
                "it already has an [mcp_servers.reviewq] table that ReviewQ did not write".into(),
                "rename or remove that table",
            ),
        }
    }
}
