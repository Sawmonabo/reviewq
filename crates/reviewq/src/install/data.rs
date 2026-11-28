//! The data ReviewQ owns in its three folders: one list, read by the System page's size and
//! by `reviewq uninstall --delete-data`, so the two never disagree. The list names what the
//! current code writes; anything else in the folders stays, as do links and entries owned by
//! another user.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::paths::Paths;

/// How an owned name is matched in its folder.
#[derive(Clone, Copy)]
enum Name {
    /// This exact name, a file or a folder with all it holds.
    Exact(&'static str),
    /// Any name with this prefix, a file or a folder with all it holds.
    Prefix(&'static str),
    /// This folder, removed only when it is already empty.
    EmptyFolder(&'static str),
}

/// `$XDG_DATA_HOME/reviewq`: the store and its sidecars, the service descriptor, store and
/// diagram scratch, checkout grants, and the empty `releases/` folder an uninstall leaves.
const DATA: &[Name] = &[
    Name::Exact("reviewq.sqlite3"),
    Name::Exact("reviewq.sqlite3-wal"),
    Name::Exact("reviewq.sqlite3-shm"),
    Name::Exact("reviewq.sqlite3-journal"),
    Name::Exact("reviewq.sqlite3.findings-display"),
    Name::Exact("reviewq.sqlite3.findings-display-journal"),
    Name::Exact("service.json"),
    Name::Prefix(".service."),
    Name::Prefix(".reviewq-stage-"),
    Name::Exact("diagram-cache"),
    Name::Exact("diagram-jobs"),
    Name::Exact("checkout-grants"),
    Name::EmptyFolder("releases"),
];
/// `$XDG_CONFIG_HOME/reviewq`.
const CONFIG: &[Name] = &[Name::Exact("runtime.json")];
/// The install record, in the state folder. A deletion removes it last, so one that stops
/// part way still finds the store it was deleting.
const RECORD: &str = "install.json";
/// `$XDG_STATE_HOME/reviewq`. `install.lock` stays: it outlives every installation.
const STATE: &[Name] = &[Name::Exact(RECORD), Name::Exact("codex-hooks.json")];
/// Interrupted atomic writes and link swaps, in any of the three folders.
const TEMPORARY: &[Name] = &[
    Name::Prefix(".reviewq-write."),
    Name::Prefix(".reviewq-link."),
];

/// The folders the owned data lives in.
#[derive(Clone)]
pub(crate) struct Folders {
    data: PathBuf,
    config: PathBuf,
    state: Option<PathBuf>,
}

/// What a deletion removed and what it left, each kept entry with the reason.
#[derive(Default)]
pub(crate) struct Deletion {
    pub deleted: Vec<PathBuf>,
    pub kept: Vec<(PathBuf, &'static str)>,
}

impl Folders {
    /// The installed profile's folders.
    pub(crate) fn installed(paths: &Paths) -> Self {
        Self {
            data: paths.profile.clone(),
            config: paths.config.clone(),
            state: Some(paths.state.clone()),
        }
    }

    /// The folders of a profile other than the installed one, which keeps its runtime
    /// settings beside its data and has no install state.
    pub(crate) fn profile(profile: &Path) -> Self {
        Self {
            data: profile.to_path_buf(),
            config: profile.to_path_buf(),
            state: None,
        }
    }

    fn lists(&self) -> Vec<(&Path, &'static [Name])> {
        let mut lists = vec![(self.data.as_path(), DATA), (self.config.as_path(), CONFIG)];
        if let Some(state) = &self.state {
            lists.push((state.as_path(), STATE));
        }
        lists
    }

    fn roots(&self) -> Vec<&Path> {
        let mut roots = vec![self.data.as_path()];
        for root in [Some(self.config.as_path()), self.state.as_deref()]
            .into_iter()
            .flatten()
        {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        roots
    }

    /// The owned entries present now, each with how it is removed, and the folders passed
    /// over, each with the reason: a folder that is a link or another user's is not read.
    fn present(&self) -> Result<(Vec<(PathBuf, Name)>, Deletion), std::io::Error> {
        let mut found = Vec::new();
        let mut passed = Deletion::default();
        for root in self.roots() {
            match root.symlink_metadata() {
                Ok(metadata) => {
                    if let Some(reason) = refusal(&metadata) {
                        passed.kept.push((root.to_path_buf(), reason));
                        continue;
                    }
                }
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
            let names = self
                .lists()
                .into_iter()
                .filter(|(folder, _)| *folder == root)
                .flat_map(|(_, names)| names.iter().copied())
                .chain(TEMPORARY.iter().copied())
                .collect::<Vec<_>>();
            let entries = match fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for entry in entries {
                let entry = entry?;
                let file_name = entry.file_name();
                let Some(name) = file_name.to_str() else {
                    continue;
                };
                if let Some(rule) = names.iter().find(|rule| rule.matches(name)) {
                    found.push((entry.path(), *rule));
                }
            }
        }
        found.sort_by(|left, right| left.0.cmp(&right.0));
        // The record goes last.
        if let Some(state) = &self.state {
            let record = state.join(RECORD);
            found.sort_by_key(|(path, _)| *path == record);
        }
        Ok((found, passed))
    }

    /// The total size of the owned data, counting files only and never following a link.
    pub(crate) fn size(&self) -> Option<u64> {
        let mut total = 0_u64;
        for (path, _) in self.present().ok()?.0 {
            total = total.checked_add(tree_size(&path).ok()?)?;
        }
        Some(total)
    }

    /// Deletes the owned data, the install record last unless `keep_record`, then each
    /// ReviewQ folder left empty.
    pub(crate) fn delete(&self, keep_record: bool) -> Result<Deletion, std::io::Error> {
        let (found, mut deletion) = self.present()?;
        let record = self.state.as_ref().map(|state| state.join(RECORD));
        for (path, rule) in found {
            if keep_record && Some(&path) == record.as_ref() {
                continue;
            }
            let removed = match rule {
                Name::EmptyFolder(_) => remove_empty(&path, &mut deletion),
                Name::Exact(_) | Name::Prefix(_) => remove_tree(&path, &mut deletion),
            };
            if removed {
                deletion.deleted.push(path);
            }
        }
        for root in self.roots() {
            if deletion.kept.iter().any(|(kept, _)| kept == root) {
                continue;
            }
            if fs::read_dir(root).is_ok_and(|mut entries| entries.next().is_none())
                && fs::remove_dir(root).is_ok()
            {
                deletion.deleted.push(root.to_path_buf());
            }
        }
        Ok(deletion)
    }
}

impl Name {
    fn matches(self, name: &str) -> bool {
        match self {
            Self::Exact(exact) | Self::EmptyFolder(exact) => name == exact,
            Self::Prefix(prefix) => name.len() > prefix.len() && name.starts_with(prefix),
        }
    }
}

/// Why `path` must stay, or `None` when it is the user's own file or folder.
fn refusal(metadata: &fs::Metadata) -> Option<&'static str> {
    if metadata.file_type().is_symlink() {
        Some("it is a symbolic link")
    } else if metadata.uid() != rustix::process::getuid().as_raw() {
        Some("it belongs to another user")
    } else {
        None
    }
}

fn tree_size(path: &Path) -> Result<u64, std::io::Error> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        total = total.saturating_add(tree_size(&entry?.path())?);
    }
    Ok(total)
}

/// Removes `path` and everything under it that is the user's own, keeping links and other
/// users' entries, and with them the folders that hold them. Returns whether `path` is gone.
fn remove_tree(path: &Path, deletion: &mut Deletion) -> bool {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return true,
        Err(_) => {
            deletion
                .kept
                .push((path.to_path_buf(), "it cannot be read"));
            return false;
        }
    };
    if let Some(reason) = refusal(&metadata) {
        deletion.kept.push((path.to_path_buf(), reason));
        return false;
    }
    if !metadata.is_dir() {
        return keep_on_error(fs::remove_file(path), path, deletion);
    }
    let Ok(entries) = fs::read_dir(path) else {
        deletion
            .kept
            .push((path.to_path_buf(), "it cannot be read"));
        return false;
    };
    let mut emptied = true;
    for entry in entries {
        emptied &= entry.is_ok_and(|entry| remove_tree(&entry.path(), deletion));
    }
    emptied && keep_on_error(fs::remove_dir(path), path, deletion)
}

/// Removes the folder at `path` when it is the user's own and holds nothing.
fn remove_empty(path: &Path, deletion: &mut Deletion) -> bool {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) => return error.kind() == ErrorKind::NotFound,
    };
    let reason =
        refusal(&metadata).or_else(|| (!metadata.is_dir()).then_some("it is not a folder"));
    if let Some(reason) = reason {
        deletion.kept.push((path.to_path_buf(), reason));
        return false;
    }
    if fs::remove_dir(path).is_err() {
        deletion.kept.push((path.to_path_buf(), "it is not empty"));
        return false;
    }
    true
}

fn keep_on_error(result: std::io::Result<()>, path: &Path, deletion: &mut Deletion) -> bool {
    match result {
        Ok(()) => true,
        Err(error) if error.kind() == ErrorKind::NotFound => true,
        Err(_) => {
            deletion
                .kept
                .push((path.to_path_buf(), "it could not be removed"));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::Folders;

    #[test]
    fn deleting_data_removes_only_the_owned_list() {
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join(".local/share/reviewq");
        let config = home.path().join(".config/reviewq");
        let state = home.path().join(".local/state/reviewq");
        let outside = home.path().join("outside");
        // A linked config folder, as a dotfiles setup makes it, is passed over.
        let linked_config = outside.join("config");
        for folder in [
            &data,
            &state,
            &linked_config,
            home.path().join(".config").as_path(),
        ] {
            fs::create_dir_all(folder).unwrap();
        }
        symlink(&linked_config, &config).unwrap();
        let owned = [
            data.join("reviewq.sqlite3"),
            data.join("reviewq.sqlite3-wal"),
            data.join("reviewq.sqlite3.findings-display"),
            data.join("service.json"),
            data.join(".service.0123"),
            data.join(".reviewq-stage-abc/owner"),
            data.join("diagram-cache/one.svg"),
            data.join("checkout-grants/grant.lock"),
            data.join(".reviewq-write.x1"),
            state.join("install.json"),
            state.join("codex-hooks.json"),
        ];
        for file in &owned {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"owned").unwrap();
        }
        fs::create_dir(data.join("releases")).unwrap();
        let kept = [
            data.join("notes.txt"),
            data.join("browser-sessions.json"),
            state.join("install.lock"),
            outside.join("target.json"),
            linked_config.join("runtime.json"),
        ];
        for file in &kept {
            fs::write(file, b"kept").unwrap();
        }
        // A link under an owned name, and one inside an owned folder, both to outside files.
        symlink(outside.join("target.json"), data.join("diagram-jobs")).unwrap();
        symlink(
            outside.join("target.json"),
            data.join("checkout-grants/link"),
        )
        .unwrap();

        let folders = Folders {
            data: data.clone(),
            config: config.clone(),
            state: Some(state.clone()),
        };
        assert_eq!(folders.size(), Some(5 * owned.len() as u64));
        let deletion = folders.delete(false).unwrap();

        for file in &owned {
            assert!(
                file.symlink_metadata().is_err(),
                "{} remains",
                file.display()
            );
        }
        assert!(!data.join("releases").exists());
        assert!(config.symlink_metadata().unwrap().file_type().is_symlink());
        for file in &kept {
            assert_eq!(fs::read(file).unwrap(), b"kept");
        }
        assert!(data.join("diagram-jobs").symlink_metadata().is_ok());
        assert!(data.join("checkout-grants/link").symlink_metadata().is_ok());
        assert_eq!(deletion.kept.len(), 3);
    }
}
