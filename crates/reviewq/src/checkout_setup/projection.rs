use std::path::Path;

use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item, Table, value};

use super::Failure;
use super::files::{Changes, Edit, read};
use super::git::Checkout;

const MARKER: &str = "# Managed by ReviewQ";
const EXCLUSIONS: &str = "# ReviewQ checkout files\n/.codex/config.toml\n/.agents/skills/reviewq-manage\n/.claude/skills/reviewq-manage\n";
const SKILL: &[u8] = include_bytes!("../../../../hosts/shared/skills/reviewq-manage/SKILL.md");

pub(super) fn checkout(
    checkout: &Checkout,
    url: &str,
    bearer: &str,
    claude_home: Option<&Path>,
) -> Result<Changes, Failure> {
    let exclude = checkout.common.join("info/exclude");
    let current = read(&exclude)?;
    let mut ignored = current
        .as_ref()
        .map_or_else(Vec::new, |file| file.bytes.clone());
    if !ignored
        .windows(EXCLUSIONS.len())
        .any(|section| section == EXCLUSIONS.as_bytes())
    {
        if !ignored.is_empty() && ignored.last() != Some(&b'\n') {
            ignored.push(b'\n');
        }
        ignored.extend_from_slice(EXCLUSIONS.as_bytes());
    }
    let mut edits = vec![Edit::new(exclude, current, ignored, false)];

    let config = checkout.worktree.join(".codex/config.toml");
    let before = read(&config)?;
    let text = before
        .as_ref()
        .map(|file| std::str::from_utf8(&file.bytes))
        .transpose()?;
    let changed = codex(text.unwrap_or_default(), url, bearer)?;
    edits.push(Edit::new(config, before, changed, true));
    edits.push(skill(
        checkout
            .worktree
            .join(".agents/skills/reviewq-manage/SKILL.md"),
    )?);

    if let Some(root) = claude_home {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err("The Claude profile is not a directory.".into());
        }
        let file = root.join(".claude.json");
        let current = read(&file)?;
        let bytes = claude(
            current.as_ref().map(|file| file.bytes.as_slice()),
            &checkout.primary,
            url,
            bearer,
        )?;
        edits.push(Edit::new(file, current, bytes, true));
        edits.push(skill(
            checkout
                .primary
                .join(".claude/skills/reviewq-manage/SKILL.md"),
        )?);
    }
    Ok(Changes::new(edits))
}

fn skill(path: std::path::PathBuf) -> Result<Edit, Failure> {
    let before = read(&path)?;
    if before.as_ref().is_some_and(|file| file.bytes != SKILL) {
        return Err("The checkout already has a different reviewq-manage skill.".into());
    }
    Ok(Edit::new(path, before, SKILL.to_vec(), false))
}

fn codex(text: &str, url: &str, bearer: &str) -> Result<Vec<u8>, Failure> {
    let mut document: DocumentMut = text
        .parse()
        .map_err(|_| "Could not parse the Codex checkout config.")?;
    let configuration = document
        .as_table_mut()
        .entry("mcp_servers")
        .or_insert(Item::Table(Table::new()));
    let servers = configuration
        .as_table_like_mut()
        .ok_or("Codex mcp_servers is not a table.")?;
    if let Some(existing) = servers.get("reviewq") {
        let managed = existing
            .as_table()
            .and_then(|table| table.decor().prefix())
            .and_then(|text| text.as_str())
            .is_some_and(|text| text.lines().any(|line| line.trim() == MARKER));
        if !managed {
            return Err("The Codex checkout already defines an unmanaged reviewq server.".into());
        }
    }
    let mut headers = Table::new();
    headers.insert("Authorization", value(format!("Bearer {bearer}")));
    let mut server = Table::new();
    server.decor_mut().set_prefix(format!("\n{MARKER}\n"));
    server.insert("url", value(url));
    server.insert("http_headers", Item::Table(headers));
    servers.insert("reviewq", Item::Table(server));
    Ok(document.to_string().into_bytes())
}

fn claude(
    data: Option<&[u8]>,
    project: &Path,
    url: &str,
    bearer: &str,
) -> Result<Vec<u8>, Failure> {
    let mut config: Value = data.map_or_else(
        || Ok(json!({})),
        |bytes| {
            serde_json::from_slice(bytes).map_err(|_| "Could not parse the Claude profile config.")
        },
    )?;
    let project = project.to_str().ok_or("Expected a UTF-8 checkout path.")?;
    let checkout = section(section(&mut config, "projects")?, project)?;
    let servers = section(checkout, "mcpServers")?
        .as_object_mut()
        .ok_or("Claude mcpServers is not an object.")?;
    if let Some(existing) = servers.get("reviewq") {
        // Reconcile only a prior ReviewQ loopback projection for this exact checkout.
        let ours = existing
            .get("url")
            .and_then(Value::as_str)
            .and_then(|raw| reqwest::Url::parse(raw).ok())
            .is_some_and(|url| {
                url.path() == "/mcp"
                    && url
                        .host_str()
                        .and_then(|host| {
                            host.trim_matches(['[', ']'])
                                .parse::<std::net::IpAddr>()
                                .ok()
                        })
                        .is_some_and(|ip| ip.is_loopback())
            });
        if !ours
            || existing
                .pointer("/headers/Authorization")
                .and_then(Value::as_str)
                .is_none_or(|value| value != format!("Bearer {bearer}"))
        {
            return Err("The Claude checkout already defines a different reviewq server.".into());
        }
    }
    servers.insert(
        "reviewq".into(),
        json!({
            "type":"http", "url":url, "headers":{"Authorization":format!("Bearer {bearer}")}
        }),
    );
    let mut bytes = serde_json::to_vec_pretty(&config)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn section<'a>(value: &'a mut Value, key: &str) -> Result<&'a mut Value, Failure> {
    let map = value
        .as_object_mut()
        .ok_or("Claude config contains an incompatible entry.")?;
    Ok(map.entry(key.to_string()).or_insert_with(|| json!({})))
}
