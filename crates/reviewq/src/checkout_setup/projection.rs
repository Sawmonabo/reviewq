use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::files::{Changes, Edit, Existing, read, read_limited};
use super::git::{self, Checkout};
use super::{Failure, Unfinished, claude, codex};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod cleanup;

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

    /// Whether everything recorded names only the address `url`.
    pub(super) fn addressed(&self, url: &str) -> bool {
        self.urls.len() == 1 && self.urls.contains(url)
    }

    /// The worktrees setup wrote into, the main one first when it chose a Claude config, and
    /// that Claude config.
    pub(super) fn places(&self) -> (Vec<PathBuf>, Option<PathBuf>) {
        let mut roots = Vec::new();
        let mut claude = None;
        for owned in &self.entries {
            let root = match owned {
                Owned::Claude { file, checkout } => {
                    claude.get_or_insert_with(|| file.clone());
                    roots.insert(0, checkout.clone());
                    continue;
                }
                Owned::Codex { file } => file.ancestors().nth(2),
                _ => None,
            };
            roots.extend(root.map(Path::to_owned));
        }
        roots.dedup();
        (roots, claude)
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
                match codex::edit(before.as_ref(), urls, bearer, Some(url)) {
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
                // An entry ReviewQ cannot prove is its own is someone else's: setup leaves it
                // alone and forgets it, so it stops no checkout's setup.
                let alone = [checkout.as_path()];
                let proof = claude::Edit::prepare(file, &alone, None, &previous.urls, bearer).await;
                if !proof.is_err_and(|error| {
                    error
                        .downcast_ref::<claude::Changed>()
                        .is_some_and(|changed| changed.entry)
                }) {
                    claude_changes.entry(file).or_default().push(checkout);
                }
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
