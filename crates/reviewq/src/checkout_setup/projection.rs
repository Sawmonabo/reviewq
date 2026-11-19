use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use toml_edit::{DocumentMut, Item, Table, value};

use super::Failure;
use super::files::{Changes, Edit, Existing, read};
use super::git::{self, Checkout};

const MARKER: &str = "# Managed by ReviewQ";
const EXCLUSIONS: &str = "# ReviewQ checkout files\n/.codex/config.toml\n/.agents/skills/reviewq-manage\n/.claude/skills/reviewq-manage\n# End ReviewQ checkout files\n";
const SKILL: &[u8] = include_bytes!("../../../../hosts/shared/skills/reviewq-manage/SKILL.md");
const CODEX: &[&str] = &[
    ".codex/config.toml",
    ".agents/skills/reviewq-manage/SKILL.md",
];
const CLAUDE: &[&str] = &[".claude/skills/reviewq-manage/SKILL.md"];
const MAX_ENTRIES: usize = 300;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Inventory {
    urls: BTreeSet<String>,
    entries: BTreeSet<Owned>,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
enum Owned {
    Codex { file: PathBuf },
    Claude { file: PathBuf, checkout: PathBuf },
    Skill { file: PathBuf, digest: [u8; 32] },
    Exclusion { file: PathBuf, separator: bool },
}

pub(super) struct Plan {
    exclusions: Changes,
    clients: Changes,
    provisional: Inventory,
    current: Inventory,
}

impl Inventory {
    pub(super) fn load(path: &Path) -> Result<Self, Failure> {
        let Some(file) = read(path)? else {
            return Ok(Self::default());
        };
        if file.mode != 0o600 {
            return Err("Checkout ownership records must be private.".into());
        }
        let inventory: Self = serde_json::from_slice(&file.bytes)?;
        if inventory.entries.len() > MAX_ENTRIES || inventory.urls.len() > 4 {
            return Err("Checkout ownership records exceed the operation limit.".into());
        }
        Ok(inventory)
    }

    fn save(&self, path: &Path) -> Result<(), Failure> {
        Edit::new(
            path.to_owned(),
            read(path)?,
            serde_json::to_vec(self)?,
            true,
        )
        .apply()
    }

    pub(super) fn claude_files(&self) -> impl Iterator<Item = &Path> {
        self.entries.iter().filter_map(|entry| match entry {
            Owned::Claude { file, .. } => Some(file.as_path()),
            _ => None,
        })
    }

    pub(super) fn cleanup(&self, path: &Path, bearer: &str) -> Result<(), Failure> {
        let mut unresolved = Self {
            urls: self.urls.clone(),
            ..Self::default()
        };
        for owned in &self.entries {
            let result = self
                .removal(owned, bearer)
                .and_then(|edit| edit.map_or(Ok(()), |edit| edit.apply()));
            if result.is_err() {
                unresolved.entries.insert(owned.clone());
            }
        }
        if unresolved.entries.is_empty() {
            if let Some(before) = read(path)? {
                Edit::remove(path.to_owned(), before).apply()?;
            }
            return Ok(());
        }
        unresolved.save(path)?;
        let paths = unresolved
            .entries
            .iter()
            .map(|entry| entry.file().display().to_string())
            .collect::<Vec<_>>();
        Err(format!("Cleanup was deferred for {}.", paths.join(", ")).into())
    }

    fn removal(&self, owned: &Owned, bearer: &str) -> Result<Option<Edit>, Failure> {
        let file = owned.file();
        let Some(before) = read(file)? else {
            return Ok(None);
        };
        let updated = match owned {
            Owned::Exclusion { separator, .. } => {
                let Some(position) = find(&before.bytes, EXCLUSIONS.as_bytes()) else {
                    if find(&before.bytes, MARKER.as_bytes()).is_some()
                        || find(&before.bytes, b"# ReviewQ checkout files").is_some()
                    {
                        return Err(conflict(file));
                    }
                    return Ok(None);
                };
                let start = if *separator && position != 0 && before.bytes[position - 1] == b'\n' {
                    position - 1
                } else {
                    position
                };
                let mut updated = before.bytes.clone();
                updated.drain(start..position + EXCLUSIONS.len());
                updated
            }
            Owned::Skill { digest, .. } => {
                let actual: [u8; 32] = Sha256::digest(&before.bytes).into();
                if actual != *digest
                    && !self.entries.contains(&Owned::Skill {
                        file: file.to_owned(),
                        digest: actual,
                    })
                {
                    return Err(conflict(file));
                }
                return Ok(Some(Edit::remove(file.to_owned(), before)));
            }
            Owned::Codex { .. } => codex(Some(&before), &self.urls, bearer, None)?
                .unwrap_or_else(|| before.bytes.clone()),
            Owned::Claude { checkout, .. } => {
                claude(Some(&before), checkout, &self.urls, bearer, None)?
                    .unwrap_or_else(|| before.bytes.clone())
            }
        };
        if updated == before.bytes {
            return Ok(None);
        }
        Ok(Some(Edit::new(
            file.to_owned(),
            Some(before),
            updated,
            owned.contains_bearer(),
        )))
    }
}

impl Owned {
    fn file(&self) -> &Path {
        match self {
            Self::Claude { file, .. }
            | Self::Codex { file }
            | Self::Skill { file, .. }
            | Self::Exclusion { file, .. } => file,
        }
    }

    const fn contains_bearer(&self) -> bool {
        matches!(self, Self::Codex { .. } | Self::Claude { .. })
    }
}

pub(super) async fn require_untracked(
    checkout: &Checkout,
    claude: Option<&Path>,
) -> Result<(), Failure> {
    for root in &checkout.worktrees {
        git::require_untracked(root, CODEX).await?;
    }
    if let Some(file) = claude {
        git::require_untracked(&checkout.primary, CLAUDE).await?;
        git::private_config(file).await?;
    }
    Ok(())
}

pub(super) fn claude_config(explicit: Option<&Path>) -> Result<Option<PathBuf>, Failure> {
    let configured = explicit
        .map(Path::to_owned)
        .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from));
    if let Some(configured) = configured {
        if !configured.is_absolute() {
            return Err("The Claude profile needs an absolute directory.".into());
        }
        return Ok(Some(configured.canonicalize()?.join(".claude.json")));
    }
    let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
    else {
        return Ok(None);
    };
    let home = home.canonicalize()?;
    let config = home.join(".claude.json");
    Ok((config.try_exists()? || home.join(".local/bin/claude").is_file()).then_some(config))
}

impl Plan {
    pub(super) fn new(
        checkout: &Checkout,
        url: &str,
        bearer: &str,
        claude_file: Option<&Path>,
        previous: Inventory,
    ) -> Result<Self, Failure> {
        let mut current = Inventory {
            urls: BTreeSet::from([url.to_string()]),
            ..Inventory::default()
        };
        let file = checkout.common.join("info/exclude");
        let exclusion = exclusion(file, &previous, &mut current.entries)?;
        let mut edits = Vec::new();
        for root in &checkout.worktrees {
            let file = root.join(CODEX[0]);
            let owned = Owned::Codex { file: file.clone() };
            let before = read(&file)?;
            let empty = BTreeSet::new();
            let urls = if previous.entries.contains(&owned) {
                &previous.urls
            } else {
                &empty
            };
            let bytes = codex(before.as_ref(), urls, bearer, Some(url))
                .map_err(|_| conflict(&file))?
                .ok_or_else(|| conflict(&file))?;
            current.entries.insert(owned);
            edits.push(Edit::new(file, before, bytes, true));
            add_skill(root.join(CODEX[1]), &previous, &mut current, &mut edits)?;
        }
        if let Some(file) = claude_file {
            let file = file.to_owned();
            let owned = Owned::Claude {
                file: file.clone(),
                checkout: checkout.primary.clone(),
            };
            let before = read(&file)?;
            let empty = BTreeSet::new();
            let urls = if previous.entries.contains(&owned) {
                &previous.urls
            } else {
                &empty
            };
            let bytes = claude(before.as_ref(), &checkout.primary, urls, bearer, Some(url))
                .map_err(|_| conflict(&file))?
                .ok_or_else(|| conflict(&file))?;
            current.entries.insert(owned);
            edits.push(Edit::new(file, before, bytes, true));
            add_skill(
                checkout.primary.join(CLAUDE[0]),
                &previous,
                &mut current,
                &mut edits,
            )?;
        }
        for owned in previous
            .entries
            .difference(&current.entries)
            .filter(|entry| {
                !current.entries.iter().any(|other| {
                    other.file() == entry.file()
                        && matches!((other, entry), (Owned::Skill { .. }, Owned::Skill { .. }))
                })
            })
        {
            if let Some(edit) = previous
                .removal(owned, bearer)
                .map_err(|_| conflict(owned.file()))?
            {
                edits.push(edit);
            }
        }
        let provisional = Inventory {
            urls: previous.urls.union(&current.urls).cloned().collect(),
            entries: previous.entries.union(&current.entries).cloned().collect(),
        };
        if provisional.entries.len() > MAX_ENTRIES || provisional.urls.len() > 4 {
            return Err("Checkout ownership changes exceed the operation limit.".into());
        }
        Ok(Self {
            exclusions: Changes::new(exclusion.into_iter().collect()),
            clients: Changes::new(edits),
            provisional,
            current,
        })
    }

    pub(super) async fn apply(
        &mut self,
        inventory: &Path,
        checkout: &Checkout,
        claude: Option<&Path>,
    ) -> Result<(), Failure> {
        self.provisional.save(inventory)?;
        self.exclusions.apply()?;
        let result = async {
            for root in &checkout.worktrees {
                git::require_ignored(root, CODEX).await?;
            }
            if let Some(file) = claude {
                git::require_ignored(&checkout.primary, CLAUDE).await?;
                git::private_config(file).await?;
            }
            require_untracked(checkout, claude).await?;
            self.clients.apply()
        }
        .await;
        if result.is_err() {
            self.exclusions.rollback()?;
        }
        result
    }

    pub(super) fn finish(self, path: &Path) -> Result<(), Failure> {
        self.current.save(path)
    }

    pub(super) fn rollback(&mut self) -> Result<(), Failure> {
        let clients = self.clients.rollback();
        let exclusions = self.exclusions.rollback();
        clients.and(exclusions)
    }
}

fn exclusion(
    file: PathBuf,
    previous: &Inventory,
    next: &mut BTreeSet<Owned>,
) -> Result<Option<Edit>, Failure> {
    let before = read(&file)?;
    let mut bytes = before
        .as_ref()
        .map_or_else(Vec::new, |file| file.bytes.clone());
    let ours = previous
        .entries
        .iter()
        .find(|entry| matches!(entry, Owned::Exclusion { file: owned, .. } if *owned == file));
    if find(&bytes, EXCLUSIONS.as_bytes()).is_some() {
        next.extend(ours.cloned());
        return Ok(None);
    }
    if find(&bytes, b"# ReviewQ checkout files").is_some() {
        return Err(conflict(&file));
    }
    let separator = !bytes.is_empty() && bytes.last() != Some(&b'\n');
    if separator {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(EXCLUSIONS.as_bytes());
    next.insert(ours.cloned().unwrap_or_else(|| Owned::Exclusion {
        file: file.clone(),
        separator,
    }));
    Ok(Some(Edit::new(file, before, bytes, false)))
}

fn add_skill(
    file: PathBuf,
    previous: &Inventory,
    current: &mut Inventory,
    edits: &mut Vec<Edit>,
) -> Result<(), Failure> {
    let before = read(&file)?;
    let owned = previous
        .entries
        .iter()
        .find(|entry| matches!(entry, Owned::Skill { file: path, .. } if *path == file));
    let digest: [u8; 32] = Sha256::digest(SKILL).into();
    if let Some(existing) = &before {
        if existing.bytes == SKILL {
            if owned.is_some() {
                current.entries.insert(Owned::Skill { file, digest });
            }
            return Ok(());
        }
        if !previous.entries.contains(&Owned::Skill {
            file: file.clone(),
            digest: Sha256::digest(&existing.bytes).into(),
        }) {
            return Err(conflict(&file));
        }
    }
    current.entries.insert(Owned::Skill {
        file: file.clone(),
        digest,
    });
    edits.push(Edit::new(file, before, SKILL.to_vec(), false));
    Ok(())
}

fn find(haystack: &[u8], piece: &[u8]) -> Option<usize> {
    haystack
        .windows(piece.len())
        .position(|section| section == piece)
}

fn conflict(file: &Path) -> Failure {
    format!(
        "ReviewQ setup could not modify {}. Its existing contents were preserved.",
        file.display()
    )
    .into()
}

fn codex(
    before: Option<&Existing>,
    owned: &BTreeSet<String>,
    bearer: &str,
    url: Option<&str>,
) -> Result<Option<Vec<u8>>, Failure> {
    let mut document: DocumentMut = before
        .map_or(Ok(""), |file| std::str::from_utf8(&file.bytes))?
        .parse()
        .map_err(|_| "Could not parse the Codex checkout config.")?;
    let configuration = document.as_table_mut();
    if url.is_none() && configuration.get("mcp_servers").is_none() {
        return Ok(None);
    }
    let servers = configuration
        .entry("mcp_servers")
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or("Codex mcp_servers is not a table.")?;
    let authorization = format!("Bearer {bearer}");
    if let Some(existing) = servers.get("reviewq") {
        let table = existing
            .as_table()
            .ok_or("The existing Codex MCP entry was preserved.")?;
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
            return Err("The existing Codex MCP entry was preserved.".into());
        }
        if old_url == url {
            return Ok(before.map(|file| file.bytes.clone()));
        }
    } else if url.is_none() {
        return Ok(None);
    }
    if let Some(url) = url {
        let mut headers = Table::new();
        headers.insert("Authorization", value(authorization));
        let mut server = Table::new();
        server.decor_mut().set_prefix(format!("\n{MARKER}\n"));
        server.insert("url", value(url));
        server.insert("http_headers", Item::Table(headers));
        servers.insert("reviewq", Item::Table(server));
    } else {
        servers.remove("reviewq");
    }
    Ok(Some(document.to_string().into_bytes()))
}

fn claude(
    before: Option<&Existing>,
    checkout: &Path,
    owned: &BTreeSet<String>,
    bearer: &str,
    url: Option<&str>,
) -> Result<Option<Vec<u8>>, Failure> {
    let mut config: Value = before.map_or_else(
        || Ok(json!({})),
        |file| {
            serde_json::from_slice(&file.bytes)
                .map_err(|_| "Could not parse the Claude profile config.")
        },
    )?;
    let checkout = checkout.to_str().ok_or("Expected a UTF-8 checkout path.")?;
    let existing = config
        .get("projects")
        .and_then(|value| value.get(checkout))
        .and_then(|value| value.get("mcpServers"))
        .and_then(|servers| servers.get("reviewq"));
    if let Some(existing) = existing {
        let value = existing
            .get("url")
            .and_then(Value::as_str)
            .ok_or("The existing Claude MCP entry was preserved.")?;
        if !owned.contains(value) || *existing != claude_entry(value, bearer) {
            return Err("The existing Claude MCP entry was preserved.".into());
        }
        if Some(value) == url {
            return Ok(before.map(|file| file.bytes.clone()));
        }
    } else if url.is_none() {
        return Ok(None);
    }
    let checkout = section(section(&mut config, "projects")?, checkout)?;
    let servers = section(checkout, "mcpServers")?
        .as_object_mut()
        .ok_or("Claude mcpServers is not an object.")?;
    if let Some(url) = url {
        servers.insert("reviewq".into(), claude_entry(url, bearer));
    } else {
        servers.remove("reviewq");
    }
    let mut bytes = serde_json::to_vec_pretty(&config)?;
    bytes.push(b'\n');
    Ok(Some(bytes))
}

fn claude_entry(url: &str, bearer: &str) -> Value {
    json!({"type":"http", "url":url, "headers":{"Authorization":format!("Bearer {bearer}")}})
}

fn section<'a>(value: &'a mut Value, key: &str) -> Result<&'a mut Value, Failure> {
    let map = value
        .as_object_mut()
        .ok_or("Claude config contains an incompatible entry.")?;
    Ok(map.entry(key.to_string()).or_insert_with(|| json!({})))
}
