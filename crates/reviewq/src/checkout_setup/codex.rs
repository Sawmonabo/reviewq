//! ReviewQ's `[mcp_servers.reviewq]` table in a Codex config: writing it, changing its URL
//! and removing it, each only where its marker, a recorded URL and the bearer prove it
//! is ReviewQ's.

use std::collections::BTreeSet;

use toml_edit::{DocumentMut, Item, Table, value};

use super::MARKER;
use super::files::Existing;

/// The Codex config `before` with ReviewQ's table at `url`, or without it when `url` is
/// `None`, and `None` then when it has no such table. `owned` holds the URLs ReviewQ wrote.
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
    // An `mcp_servers` table setup adds is implicit, with no header of its own, so removing
    // ReviewQ's table removes it too and leaves no `[mcp_servers]` line behind.
    let servers = configuration
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut servers = Table::new();
            servers.set_implicit(true);
            Item::Table(servers)
        })
        .as_table_like_mut()
        .ok_or(Refusal::Servers)?;
    let authorization = format!("Bearer {bearer}");
    if let Some(existing) = servers.get("reviewq") {
        let table = existing.as_table().ok_or(Refusal::Entry)?;
        let marker = table
            .decor()
            .prefix()
            .and_then(|text| text.as_str())
            .is_some_and(|text| text.lines().any(|line| line.trim() == MARKER));
        let old_url = table.get("url").and_then(Item::as_str);
        let headers = table.get("http_headers").and_then(Item::as_table);
        if !marker
            || table.len() != 2
            || old_url.is_none_or(|value| !owned.contains(value))
            || headers.is_none_or(|headers| {
                headers.len() != 1
                    || headers.get("Authorization").and_then(Item::as_str) != Some(&authorization)
            })
        {
            return Err(Refusal::Entry);
        }
        if old_url == url {
            return Ok(before.map(|file| file.bytes.clone()));
        }
    } else if url.is_none() {
        return Ok(None);
    }
    if let Some(url) = url {
        // ReviewQ's table keeps its place and the lines above its header; only its URL changes.
        if let Some(table) = servers.get_mut("reviewq").and_then(Item::as_table_mut) {
            table.insert("url", value(url));
            return Ok(Some(document.to_string().into_bytes()));
        }
        let mut headers = Table::new();
        headers.insert("Authorization", value(authorization));
        let mut server = Table::new();
        server.decor_mut().set_prefix(format!("\n{MARKER}\n"));
        server.insert("url", value(url));
        server.insert("http_headers", Item::Table(headers));
        servers.insert("reviewq", Item::Table(server));
    } else {
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
    }
    Ok(Some(document.to_string().into_bytes()))
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
    // ReviewQ's marker line, and the blank line setup writes before it.
    if let Some(marker) = lines.iter().position(|line| line.trim() == MARKER) {
        let blank = marker > 0 && lines[marker - 1].trim().is_empty();
        lines.drain(marker - usize::from(blank)..=marker);
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
