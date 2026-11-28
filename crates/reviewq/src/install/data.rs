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
/// `$XDG_STATE_HOME/reviewq`. The installation lock's file is not listed: only the uninstall
/// that holds it deletes it, after everything else.
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
    /// The installation lock's file, in the state folder.
    lock: Option<PathBuf>,
}

/// What a deletion removed and what it left, each kept entry with the reason.
#[derive(Default)]
pub(crate) struct Deletion {
    pub deleted: Vec<PathBuf>,
    pub kept: Vec<(PathBuf, Kept)>,
}

/// Why a deletion left an entry in place.
#[derive(Clone, Copy)]
pub(crate) enum Kept {
    Link,
    OtherUser,
    NotFolder,
    NotEmpty,
    Unreadable,
    NotRemoved,
    /// A ReviewQ folder that holds entries ReviewQ does not own.
    HoldsOthers,
}

impl Kept {
    /// The reason, as it completes "Kept <path> because …".
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Link => "it is a symbolic link",
            Self::OtherUser => "it belongs to another user",
            Self::NotFolder => "it is not a folder",
            Self::NotEmpty => "it is not empty",
            Self::Unreadable => "it cannot be read",
            Self::NotRemoved => "it could not be removed",
            Self::HoldsOthers => "it holds files that are not ReviewQ's",
        }
    }

    /// Whether removing the entry failed, rather than a rule keeping it.
    pub(crate) fn failed(self) -> bool {
        matches!(self, Self::Unreadable | Self::NotRemoved)
    }
}

/// One walk over the owned entries: a deletion, or a measure of what a deletion would
/// remove. Both follow the same rules, so the System page's size never counts a kept entry.
#[derive(Default)]
struct Walk {
    /// Remove entries, rather than only count their bytes.
    remove: bool,
    bytes: u64,
    deletion: Deletion,
}

impl Folders {
    /// The installed profile's folders.
    pub(crate) fn installed(paths: &Paths) -> Self {
        Self {
            data: paths.profile.clone(),
            config: paths.config.clone(),
            state: Some(paths.state.clone()),
            lock: Some(paths.lock_path()),
        }
    }

    /// The folders of a profile other than the installed one, which keeps its runtime
    /// settings beside its data and has no install state.
    pub(crate) fn profile(profile: &Path) -> Self {
        Self {
            data: profile.to_path_buf(),
            config: profile.to_path_buf(),
            state: None,
            lock: None,
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

    fn record(&self) -> Option<PathBuf> {
        self.state.as_ref().map(|state| state.join(RECORD))
    }

    /// The names ReviewQ owns directly in `root`.
    fn names(&self, root: &Path) -> Vec<Name> {
        self.lists()
            .into_iter()
            .filter(|(folder, _)| *folder == root)
            .flat_map(|(_, names)| names.iter().copied())
            .chain(TEMPORARY.iter().copied())
            .collect()
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
            let names = self.names(root);
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
        if let Some(record) = self.record() {
            found.sort_by_key(|(path, _)| *path == record);
        }
        Ok((found, passed))
    }

    /// Walks the owned entries, passing over the install record when `keep_record`.
    fn walk(&self, remove: bool, keep_record: bool) -> Result<Walk, std::io::Error> {
        let (found, deletion) = self.present()?;
        let mut walk = Walk {
            remove,
            bytes: 0,
            deletion,
        };
        let record = self.record();
        for (path, rule) in found {
            if keep_record && Some(&path) == record.as_ref() {
                continue;
            }
            let removed = match rule {
                Name::EmptyFolder(_) => walk.empty_folder(&path),
                Name::Exact(_) | Name::Prefix(_) => walk.tree(&path),
            };
            if removed {
                walk.deletion.deleted.push(path);
            }
        }
        Ok(walk)
    }

    /// The total size of what a deletion would remove, counting files only and never
    /// following a link.
    pub(crate) fn size(&self) -> Option<u64> {
        Some(self.walk(false, false).ok()?.bytes)
    }

    /// Deletes the owned data, the install record last unless `keep_record`, then each
    /// ReviewQ folder left empty. A folder that still holds entries ReviewQ does not own is
    /// kept with that reason; one that holds only owned entries, already reported, the install
    /// record or the installation lock's file, is not.
    pub(crate) fn delete(&self, keep_record: bool) -> Result<Deletion, std::io::Error> {
        let mut deletion = self.walk(true, keep_record)?.deletion;
        self.remove_empty_roots(&mut deletion, true);
        Ok(deletion)
    }

    /// Deletes the install record a deletion kept, then each ReviewQ folder left empty. The
    /// deletion already reported the folders it kept.
    pub(crate) fn delete_record(&self) -> Deletion {
        let mut walk = Walk {
            remove: true,
            ..Walk::default()
        };
        if let Some(record) = self.record()
            && record.symlink_metadata().is_ok()
            && walk.tree(&record)
        {
            walk.deletion.deleted.push(record);
        }
        self.remove_empty_roots(&mut walk.deletion, false);
        walk.deletion
    }

    /// Deletes the installation lock's file while `lock` still holds it, then the state
    /// folder when that leaves it empty, and releases the lock. Only the last act of an
    /// uninstall that deleted everything else does this; that deletion already removed the
    /// other folders it left empty, so only the state folder's removal follows the unlink. A
    /// process that was waiting for the lock then finds the file gone and locks a new one. In
    /// between, a process that starts now can create and lock a new file before this one lets
    /// go. That is safe: the new file keeps the state folder, which is removed only when
    /// empty, and an open that finds the folder already gone makes it again and retries.
    pub(crate) fn delete_lock(&self, lock: fs::File) -> Deletion {
        let mut walk = Walk {
            remove: true,
            ..Walk::default()
        };
        if let Some(path) = &self.lock
            && path.symlink_metadata().is_ok()
            && walk.tree(path)
        {
            walk.deletion.deleted.push(path.clone());
        }
        if let Some(state) = &self.state {
            self.remove_empty(&[state.as_path()], &mut walk.deletion, false);
        }
        drop(lock);
        walk.deletion
    }

    /// Removes each ReviewQ folder left empty. With `report`, keeps each one that holds an
    /// entry ReviewQ does not own, with that reason.
    fn remove_empty_roots(&self, deletion: &mut Deletion, report: bool) {
        self.remove_empty(&self.roots(), deletion, report);
    }

    /// [`Self::remove_empty_roots`] for the given ReviewQ folders only.
    fn remove_empty(&self, roots: &[&Path], deletion: &mut Deletion, report: bool) {
        for &root in roots {
            // A folder that is a link or another user's stays, as its entries would.
            if root
                .symlink_metadata()
                .is_ok_and(|metadata| refusal(&metadata).is_some())
            {
                continue;
            }
            let Ok(entries) = fs::read_dir(root) else {
                continue;
            };
            let names = self.names(root);
            let (mut empty, mut others) = (true, false);
            for entry in entries.flatten() {
                empty = false;
                others |= self.lock.as_ref() != Some(&entry.path())
                    && !entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| names.iter().any(|rule| rule.matches(name)));
            }
            if empty && fs::remove_dir(root).is_ok() {
                deletion.deleted.push(root.to_path_buf());
            } else if report && others {
                deletion.kept.push((root.to_path_buf(), Kept::HoldsOthers));
            }
        }
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
fn refusal(metadata: &fs::Metadata) -> Option<Kept> {
    if metadata.file_type().is_symlink() {
        Some(Kept::Link)
    } else if metadata.uid() != rustix::process::getuid().as_raw() {
        Some(Kept::OtherUser)
    } else {
        None
    }
}

impl Walk {
    fn keep(&mut self, path: &Path, reason: Kept) -> bool {
        self.deletion.kept.push((path.to_path_buf(), reason));
        false
    }

    /// Removes, or counts, `path` and everything under it that is the user's own, keeping
    /// links and other users' entries, and with them the folders that hold them. Returns
    /// whether `path` is, or would be, gone.
    fn tree(&mut self, path: &Path) -> bool {
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return true,
            Err(_) => return self.keep(path, Kept::Unreadable),
        };
        if let Some(reason) = refusal(&metadata) {
            return self.keep(path, reason);
        }
        if !metadata.is_dir() {
            if !self.remove {
                self.bytes = self.bytes.saturating_add(metadata.len());
                return true;
            }
            return self.removed(fs::remove_file(path), path);
        }
        let Ok(entries) = fs::read_dir(path) else {
            return self.keep(path, Kept::Unreadable);
        };
        let mut emptied = true;
        for entry in entries {
            emptied &= entry.is_ok_and(|entry| self.tree(&entry.path()));
        }
        emptied && (!self.remove || self.removed(fs::remove_dir(path), path))
    }

    /// Removes the folder at `path` when it is the user's own and holds nothing. It adds
    /// nothing to a measure.
    fn empty_folder(&mut self, path: &Path) -> bool {
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) => return error.kind() == ErrorKind::NotFound,
        };
        if let Some(reason) =
            refusal(&metadata).or_else(|| (!metadata.is_dir()).then_some(Kept::NotFolder))
        {
            return self.keep(path, reason);
        }
        if self.remove && fs::remove_dir(path).is_err() {
            return self.keep(path, Kept::NotEmpty);
        }
        true
    }

    fn removed(&mut self, result: std::io::Result<()>, path: &Path) -> bool {
        match result {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => true,
            Err(_) => self.keep(path, Kept::NotRemoved),
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
            lock: Some(state.join("install.lock")),
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
        // Three links, and the data folder for the files that are not ReviewQ's. The state
        // folder, holding only the lock's file, needs no line.
        let left = deletion
            .kept
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            left,
            [
                config,
                data.join("checkout-grants/link"),
                data.join("diagram-jobs"),
                data.clone(),
            ]
        );
    }
}
