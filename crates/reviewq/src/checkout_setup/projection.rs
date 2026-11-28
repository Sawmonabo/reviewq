use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use toml_edit::{DocumentMut, Item, Table, value};

use super::files::{self, Changes, Edit, Existing, read, read_limited};
use super::git::{self, Checkout};
use super::{Failure, Unfinished, claude};

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
    claude: Vec<claude::Edit>,
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

    pub(super) async fn has_worktree(&self, worktree: &Path) -> Result<bool, Failure> {
        let file = worktree.join(CODEX[0]);
        let ready = self.entries.contains(&Owned::Codex { file: file.clone() })
            && read(&file)?.is_some_and(|file| file.mode == 0o600)
            && read(&worktree.join(CODEX[1]))?.is_some_and(|file| file.bytes == SKILL);
        if ready {
            git::require_untracked(worktree, CODEX).await?;
            git::require_ignored(worktree, CODEX).await?;
        }
        Ok(ready)
    }

    /// Removes what the records say ReviewQ wrote, and forgets an entry once nothing of
    /// ReviewQ's is left there. What stays is recorded, and the error says why, what the user
    /// removes from which file, then `again`, the step that finishes the command.
    pub(super) async fn cleanup(
        &self,
        path: &Path,
        common: &Path,
        bearer: &str,
        again: &str,
    ) -> Result<(), Failure> {
        let mut unresolved = Self {
            urls: self.urls.clone(),
            ..Self::default()
        };
        let mut left = Unfinished::new(again);
        for owned in &self.entries {
            if matches!(owned, Owned::Exclusion { .. }) {
                let dependencies = unresolved
                    .entries
                    .iter()
                    .filter(|entry| {
                        matches!(entry,
                            Owned::Codex { file } if file.try_exists().unwrap_or(true)
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                // An exclusion waits only for the files it hides, so it needs no step.
                if !dependencies.is_empty() {
                    unresolved.entries.extend(dependencies);
                    unresolved.entries.insert(owned.clone());
                    continue;
                }
            }
            if let Err(kept) = self.remove_entry(owned, common, bearer).await {
                unresolved.entries.insert(owned.clone());
                kept.add_to(owned, &mut left);
            }
        }
        if unresolved.entries.is_empty() {
            if let Some(before) = read(path)? {
                Edit::remove(path.to_owned(), before).apply()?;
            }
            return Ok(());
        }
        unresolved.save(path)?;
        left.result()
    }

    /// Removes one entry's setup. `Ok` when nothing of ReviewQ's is left there.
    async fn remove_entry(&self, owned: &Owned, common: &Path, bearer: &str) -> Result<(), Kept> {
        let Owned::Claude { file, checkout } = owned else {
            return match self.local_removal(owned, common, bearer).await? {
                Some(edit) => edit.apply().map_err(Kept::Failed),
                None => Ok(()),
            };
        };
        let removed = claude::Edit::prepare(file, &[checkout], None, &self.urls, bearer)
            .await
            .and_then(|mut edit| edit.apply());
        let Err(error) = removed else {
            return Ok(());
        };
        match error.downcast::<claude::Changed>() {
            // The entry names the checkout's folder. Once that folder holds another repository,
            // or none, an entry there that is not ReviewQ's belongs to someone else.
            Ok(changed) if changed.entry => {
                if git::belongs(checkout, common).await? {
                    Err(Kept::Edited)
                } else {
                    Ok(())
                }
            }
            // The file itself, such as JSON that does not parse, may still hold ReviewQ's entry.
            Ok(changed) => Err(Kept::Unchanged {
                cause: changed.cause,
                step: changed.step,
            }),
            Err(error) => Err(Kept::Failed(error)),
        }
    }

    /// The edit that removes what ReviewQ wrote at a file in the checkout, or `None` when
    /// nothing of ReviewQ's is left there: the file is gone or is no longer the one setup
    /// wrote, its folder is no longer a worktree of the checkout, or it no longer holds
    /// ReviewQ's part. Nothing is read from a folder that is no longer the checkout's. When Git
    /// cannot say where a Codex config stands, ReviewQ still removes its table there, which its
    /// marker, a recorded URL and this key's bearer prove ReviewQ wrote, so the bearer never
    /// outlives the record; anything else waits for Git's answer.
    async fn local_removal(
        &self,
        owned: &Owned,
        common: &Path,
        bearer: &str,
    ) -> Result<Option<Edit>, Kept> {
        let file = owned.file();
        let relative = match owned {
            Owned::Codex { .. } => Some(CODEX[0]),
            Owned::Skill { .. }
                if file
                    .as_os_str()
                    .as_encoded_bytes()
                    .ends_with(CODEX[1].as_bytes()) =>
            {
                Some(CODEX[1])
            }
            Owned::Skill { .. } => Some(CLAUDE[0]),
            Owned::Exclusion { .. } => None,
            Owned::Claude { .. } => return Ok(None),
        };
        let tracked = match relative {
            Some(relative) => match git::destination(file, common, relative).await {
                Ok(git::Destination::Elsewhere) => return Ok(None),
                // A skill Git tracks is a copy the repository keeps, with no key in it.
                Ok(git::Destination::Tracked) if matches!(owned, Owned::Skill { .. }) => {
                    return Ok(None);
                }
                Ok(git::Destination::Tracked) => true,
                Ok(git::Destination::Owned) => false,
                Err(_) if matches!(owned, Owned::Codex { .. }) => false,
                Err(error) => return Err(Kept::Failed(error)),
            },
            None => false,
        };
        let Some(before) = files::read_written(file)? else {
            return Ok(None);
        };
        let updated = match owned {
            Owned::Codex { .. } => match codex(Some(&before), &self.urls, bearer, None) {
                Ok(updated) => updated.unwrap_or_else(|| before.bytes.clone()),
                Err(Refusal::Entry) => return Err(Kept::Edited),
                // An `mcp_servers` that is no table holds no table of ReviewQ's.
                Err(Refusal::Servers) => return Ok(None),
                Err(refusal) => {
                    let (cause, step) = refusal.words(true);
                    return Err(Kept::Unchanged {
                        cause,
                        step: step.to_owned(),
                    });
                }
            },
            Owned::Skill { digest, .. } => {
                let actual: [u8; 32] = Sha256::digest(&before.bytes).into();
                if actual != *digest
                    && !self.entries.contains(&Owned::Skill {
                        file: file.to_owned(),
                        digest: actual,
                    })
                {
                    return Err(Kept::Edited);
                }
                return Ok(Some(Edit::remove(file.to_owned(), before)));
            }
            Owned::Exclusion { separator, .. } if file == common.join("info/exclude") => {
                let Some(position) = find(&before.bytes, EXCLUSIONS.as_bytes()) else {
                    if find(&before.bytes, MARKER.as_bytes()).is_some()
                        || find(&before.bytes, b"# ReviewQ checkout files").is_some()
                    {
                        return Err(Kept::Edited);
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
            // A damaged record: setup writes only the common folder's exclude file.
            Owned::Exclusion { .. } => return Ok(None),
            Owned::Claude { .. } => return Ok(None),
        };
        if updated == before.bytes {
            return Ok(None);
        }
        if tracked {
            return Err(Kept::Tracked);
        }
        if matches!(owned, Owned::Codex { .. }) && updated.iter().all(u8::is_ascii_whitespace) {
            return Ok(Some(Edit::remove(file.to_owned(), before)));
        }
        Ok(Some(Edit::new(
            file.to_owned(),
            Some(before),
            updated,
            matches!(owned, Owned::Codex { .. }),
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

    /// What the user removes by hand when cleanup left this entry for them.
    fn removal(&self) -> String {
        match self {
            Self::Codex { file } => format!(
                "remove the [mcp_servers.reviewq] table from {}",
                file.display()
            ),
            Self::Claude { file, checkout } => format!(
                "remove the reviewq-manage MCP server of {} from {}",
                checkout.display(),
                file.display()
            ),
            Self::Skill { file, .. } => format!(
                "delete the folder {}",
                file.parent().unwrap_or(file).display()
            ),
            Self::Exclusion { file, .. } => format!(
                "remove the lines from \"# ReviewQ checkout files\" to \"# End ReviewQ checkout files\" in {}",
                file.display()
            ),
        }
    }
}

/// Why cleanup left an entry in place.
enum Kept {
    /// What ReviewQ wrote there was changed by hand.
    Edited,
    /// The file around it no longer reads, such as a line that is not valid TOML, so ReviewQ
    /// cannot tell what of it is ReviewQ's. `cause` and `step` are as for `Unfinished::unchanged`.
    Unchanged { cause: String, step: String },
    /// Git now tracks the file, which still holds what ReviewQ wrote, so ReviewQ leaves it.
    Tracked,
    /// Anything else, such as a file that could not be read or written, or Git not answering.
    Failed(Failure),
}

impl From<Failure> for Kept {
    fn from(error: Failure) -> Self {
        Self::Failed(error)
    }
}

impl Kept {
    /// Says in `left` why `owned` stays and what the user does about it.
    fn add_to(self, owned: &Owned, left: &mut Unfinished) {
        match self {
            Self::Edited => left.edited(owned.removal()),
            Self::Unchanged { cause, step } => left.unchanged(owned.file(), &cause, &step),
            Self::Tracked => left.step(format!(
                "Git tracks {}, so ReviewQ left its management setup there. To finish, {}.",
                owned.file().display(),
                owned.removal()
            )),
            Self::Failed(error) => {
                left.failed("remove its management setup from", owned.file(), &error);
            }
        }
    }
}

pub(super) async fn require_untracked(
    checkout: &Checkout,
    claude: Option<&Path>,
) -> Result<(), Failure> {
    if !checkout.worktrees.contains(&checkout.primary) {
        return Err("Check out a branch in the main worktree before running ReviewQ setup.".into());
    }
    for root in &checkout.worktrees {
        git::require_untracked(root, CODEX).await?;
    }
    if let Some(file) = claude {
        git::require_untracked(&checkout.primary, CLAUDE).await?;
        git::private_config(file, true).await?;
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
    pub(super) async fn new(
        checkout: &Checkout,
        url: &str,
        bearer: &str,
        claude_file: Option<&Path>,
        previous: Inventory,
        skill: &[u8],
        again: &str,
    ) -> Result<Self, Failure> {
        let mut current = Inventory {
            urls: BTreeSet::from([url.to_string()]),
            ..Inventory::default()
        };
        // Everything setup cannot change is said at once, so one more run is enough.
        let mut left = Unfinished::new(again);
        let file = checkout.common.join("info/exclude");
        let exclusion = exclusion(file, &previous, &mut current.entries, &mut left)?;
        let mut edits = Vec::new();
        let mut setup = Setup {
            previous: &previous,
            current: &mut current,
            edits: &mut edits,
            left: &mut left,
        };
        for root in &checkout.worktrees {
            let file = root.join(CODEX[0]);
            let owned = Owned::Codex { file: file.clone() };
            if let Some(before) = setup.read(&file) {
                let ours = previous.entries.contains(&owned);
                let empty = BTreeSet::new();
                let urls = if ours { &previous.urls } else { &empty };
                match codex(before.as_ref(), urls, bearer, Some(url)) {
                    Ok(bytes) => {
                        let bytes = bytes.ok_or("Codex setup produced no entry.")?;
                        setup.current.entries.insert(owned);
                        setup.edits.push(Edit::new(file, before, bytes, true));
                    }
                    Err(refusal) => {
                        let (cause, step) = refusal.words(ours);
                        setup.left.unchanged(&file, &cause, step);
                    }
                }
            }
            setup.skill(root.join(CODEX[1]), skill);
        }
        let mut claude_changes = BTreeMap::<&Path, Vec<&Path>>::new();
        if let Some(file) = claude_file {
            claude_changes.entry(file).or_default();
            setup.current.entries.insert(Owned::Claude {
                file: file.to_owned(),
                checkout: checkout.primary.clone(),
            });
            setup.skill(checkout.primary.join(CLAUDE[0]), skill);
        }
        // A file setup already left unchanged, and said why, is not named a second time.
        let dropped = previous
            .entries
            .difference(&current.entries)
            .filter(|entry| {
                !left.names(entry.file())
                    && !current.entries.iter().any(|other| {
                        other.file() == entry.file()
                            && matches!((other, entry), (Owned::Skill { .. }, Owned::Skill { .. }))
                    })
            })
            .collect::<Vec<_>>();
        for owned in dropped {
            if let Owned::Claude { file, checkout } = owned {
                claude_changes.entry(file).or_default().push(checkout);
                continue;
            }
            match previous
                .local_removal(owned, &checkout.common, bearer)
                .await
            {
                Ok(edit) => edits.extend(edit),
                Err(kept) => kept.add_to(owned, &mut left),
            }
        }
        let mut claude_edits = Vec::new();
        for (file, retired) in claude_changes {
            let selected = (Some(file) == claude_file).then(|| {
                let owned = previous.entries.contains(&Owned::Claude {
                    file: file.to_owned(),
                    checkout: checkout.primary.clone(),
                });
                (checkout.primary.as_path(), url, owned)
            });
            match claude::Edit::prepare(file, &retired, selected, &previous.urls, bearer).await {
                Ok(edit) => claude_edits.push(edit),
                Err(error) => match error.downcast::<claude::Changed>() {
                    Ok(changed) => left.unchanged(file, &changed.cause, &changed.step),
                    Err(error) => left.failed("update", file, &error),
                },
            }
        }
        left.result()?;
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
            claude: claude_edits,
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
                git::private_config(file, true).await?;
            }
            require_untracked(checkout, claude).await?;
            self.clients.apply()?;
            self.claude.iter_mut().try_for_each(claude::Edit::apply)
        }
        .await;
        if result.is_err() {
            self.rollback()?;
        }
        result
    }

    pub(super) fn finish(self, path: &Path) -> Result<(), Failure> {
        self.current.save(path)
    }

    pub(super) fn rollback(&mut self) -> Result<(), Failure> {
        let mut claude_error = None;
        for edit in self.claude.iter_mut().rev() {
            if let Err(error) = edit.rollback() {
                claude_error.get_or_insert(error);
            }
        }
        let clients = self.clients.rollback();
        if let Some(error) = claude_error {
            return Err(error);
        }
        clients?;
        if !self.clients.rollback_conflict {
            self.exclusions.rollback()?;
        }
        Ok(())
    }
}

fn exclusion(
    file: PathBuf,
    previous: &Inventory,
    next: &mut BTreeSet<Owned>,
    left: &mut Unfinished,
) -> Result<Option<Edit>, Failure> {
    let before = match read(&file) {
        Ok(before) => before,
        Err(error) => {
            left.failed("read", &file, &error);
            return Ok(None);
        }
    };
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
        let cause = if ours.is_some() {
            "the lines ReviewQ wrote there were changed by hand"
        } else {
            "it already has a line \"# ReviewQ checkout files\" that ReviewQ did not write"
        };
        left.unchanged(
            &file,
            cause,
            "remove the lines from \"# ReviewQ checkout files\" to \"# End ReviewQ checkout files\" there",
        );
        return Ok(None);
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

/// The installed release's checkout skill, which must be the one this program carries. Setup
/// with another release's program says how to make them the same, then `again`.
pub(super) fn skill(profile: &Path, again: &str) -> Result<Cow<'static, [u8]>, Failure> {
    let Some(path) = crate::install::checkout_skill(profile)? else {
        return Ok(Cow::Borrowed(SKILL));
    };
    let mut left = Unfinished::new(again);
    match read_limited(&path, 32 * 1024) {
        Ok(Some(owned)) if owned.bytes == SKILL => return Ok(Cow::Owned(owned.bytes)),
        Ok(_) => left.step(format!(
            "The installed ReviewQ's checkout skill at {} is not the one this `reviewq` program carries. To finish, use the installed `reviewq` on your PATH, or install the release this program came from with `<release folder>/bin/reviewq install --from <release folder>`.",
            path.display()
        )),
        Err(error) => left.failed("read its installed checkout skill", &path, &error),
    }
    Err(Box::new(left))
}

/// What `Plan::new` gathers: the entries and edits it plans, and what it must leave as it is.
struct Setup<'a> {
    previous: &'a Inventory,
    current: &'a mut Inventory,
    edits: &'a mut Vec<Edit>,
    left: &'a mut Unfinished,
}

impl Setup<'_> {
    /// The file at `file`, or `None` after saying in `left` why it could not be read.
    fn read(&mut self, file: &Path) -> Option<Option<Existing>> {
        read(file)
            .map_err(|error| self.left.failed("read", file, &error))
            .ok()
    }

    /// Plans the skill at `file`, unless something other than ReviewQ's skill is there.
    fn skill(&mut self, file: PathBuf, skill: &[u8]) {
        let Some(before) = self.read(&file) else {
            return;
        };
        let owned = self
            .previous
            .entries
            .iter()
            .find(|entry| matches!(entry, Owned::Skill { file: path, .. } if *path == file));
        let digest: [u8; 32] = Sha256::digest(skill).into();
        if let Some(existing) = &before {
            if existing.bytes == skill {
                if owned.is_some() {
                    self.current.entries.insert(Owned::Skill { file, digest });
                }
                return;
            }
            if !self.previous.entries.contains(&Owned::Skill {
                file: file.clone(),
                digest: Sha256::digest(&existing.bytes).into(),
            }) {
                let cause = if owned.is_some() {
                    "the skill ReviewQ wrote there was changed by hand"
                } else {
                    "it is a file ReviewQ did not write"
                };
                self.left
                    .unchanged(&file, cause, "move or delete that file");
                return;
            }
        }
        self.current.entries.insert(Owned::Skill {
            file: file.clone(),
            digest,
        });
        self.edits
            .push(Edit::new(file, before, skill.to_vec(), false));
    }
}

fn find(haystack: &[u8], piece: &[u8]) -> Option<usize> {
    haystack
        .windows(piece.len())
        .position(|section| section == piece)
}

fn codex(
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
        let mut headers = Table::new();
        headers.insert("Authorization", value(authorization));
        let mut server = Table::new();
        server.decor_mut().set_prefix(format!("\n{MARKER}\n"));
        server.insert("url", value(url));
        server.insert("http_headers", Item::Table(headers));
        servers.insert("reviewq", Item::Table(server));
    } else {
        servers.remove("reviewq");
        if configuration
            .get("mcp_servers")
            .and_then(Item::as_table)
            .is_some_and(|table| table.is_implicit() && table.is_empty())
        {
            configuration.remove("mcp_servers");
        }
    }
    Ok(Some(document.to_string().into_bytes()))
}

/// Why setup leaves a Codex config as it is.
enum Refusal {
    Text,
    Line(usize),
    Servers,
    Entry,
}

impl Refusal {
    /// The cause, as a clause about the file, and the user's step. `ours` says whether
    /// ReviewQ wrote the file's [mcp_servers.reviewq] table before.
    fn words(&self, ours: bool) -> (String, &'static str) {
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
