//! Cleanup: removing what the records say ReviewQ wrote, and saying what it left.

use std::path::Path;

use sha2::{Digest, Sha256};

use super::{CLAUDE, CODEX, EXCLUSIONS, Inventory, Owned, find};
use crate::checkout_setup::codex::{self, Refusal};
use crate::checkout_setup::files::{self, Edit, read};
use crate::checkout_setup::{Failure, MARKER, Unfinished, claude, git};

impl Inventory {
    /// Removes what the records say ReviewQ wrote, and forgets an entry once nothing of
    /// ReviewQ's is left there. What stays is recorded, and the error says why, what the user
    /// removes from which file, then `again`, the step that finishes the command.
    pub(in crate::checkout_setup) async fn cleanup(
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
    /// wrote, or it no longer holds ReviewQ's part. Where the folder is no longer a worktree of
    /// the checkout, ReviewQ still removes what proves to be its own: its Codex table, by its
    /// marker, a recorded URL and this key's bearer, and a skill whose bytes are the ones it
    /// recorded there. Anything else there is left untouched. When Git cannot say where a
    /// Codex config stands, its table goes by the same proof, so the bearer never outlives the
    /// record; anything else Git cannot place waits for its answer.
    pub(super) async fn local_removal(
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
        let codex_config = matches!(owned, Owned::Codex { .. });
        let (tracked, elsewhere) = match relative {
            Some(relative) => match git::destination(file, common, relative).await {
                Ok(git::Destination::Elsewhere) => (false, true),
                // A skill Git tracks is a copy the repository keeps, with no key in it.
                Ok(git::Destination::Tracked) if matches!(owned, Owned::Skill { .. }) => {
                    return Ok(None);
                }
                Ok(git::Destination::Tracked) => (true, false),
                Ok(git::Destination::Owned) => (false, false),
                Err(_) if codex_config => (false, false),
                Err(error) => return Err(Kept::Failed(error)),
            },
            None => (false, false),
        };
        let Some(before) = files::read_written(file)? else {
            return Ok(None);
        };
        let updated = match owned {
            Owned::Codex { .. } => match codex::edit(Some(&before), &self.urls, bearer, None) {
                Ok(updated) => updated.unwrap_or_else(|| before.bytes.clone()),
                // There, a file without the exact table ReviewQ wrote is someone else's.
                Err(_) if elsewhere => return Ok(None),
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
                    // There, a skill that is not the one ReviewQ wrote is someone else's.
                    return if elsewhere {
                        Ok(None)
                    } else {
                        Err(Kept::Edited)
                    };
                }
                return Ok(Some(Edit::remove_skill(file.to_owned(), before)));
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
pub(super) enum Kept {
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
    pub(super) fn add_to(self, owned: &Owned, left: &mut Unfinished) {
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
