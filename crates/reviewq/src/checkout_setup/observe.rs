use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, Metadata};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use reviewq_app::access::Actor;
use reviewq_app::workspace::WorkspaceStore;
use reviewq_domain::session::AbsolutePath;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use super::{Failure, files, git};

const MAX_WATCH_PATHS: usize = 1024;

pub(super) struct Topology {
    directories: Vec<PathBuf>,
    pub fingerprint: [u8; 32],
}

impl Topology {
    pub(super) fn read(common: &Path) -> Result<Self, Failure> {
        let mut directories = Vec::new();
        let mut materialized = BTreeMap::<OsString, [u8; 32]>::new();
        if metadata(common)?.filter(Metadata::is_dir).is_some() {
            directories.push(common.to_owned());
            let registrations = common.join("worktrees");
            if metadata(&registrations)?.filter(Metadata::is_dir).is_some() {
                directories.push(registrations.clone());
                for (number, entry) in fs::read_dir(registrations)?.enumerate() {
                    if number >= git::MAX_WORKTREES {
                        return Err(
                            "The repository exceeds the linked worktree observation limit.".into(),
                        );
                    }
                    let entry = entry?;
                    if !entry.file_type()?.is_dir() {
                        continue;
                    }
                    let path = entry.path();
                    directories.push(path.clone());
                    if metadata(&path.join("index"))?
                        .filter(Metadata::is_file)
                        .is_some()
                        && let Some(locator) = files::read_limited(&path.join("gitdir"), 4096)?
                    {
                        materialized
                            .insert(entry.file_name(), Sha256::digest(locator.bytes).into());
                    }
                }
            }
        }
        let mut hash = Sha256::new();
        for (name, digest) in materialized {
            let name = name.as_encoded_bytes();
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name);
            hash.update(digest);
        }
        Ok(Self {
            directories,
            fingerprint: hash.finalize().into(),
        })
    }
}

fn metadata(path: &Path) -> Result<Option<Metadata>, Failure> {
    match path.symlink_metadata() {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) struct Observers {
    pub wake: Arc<Notify>,
    watcher: Option<RecommendedWatcher>,
    roots: HashMap<AbsolutePath, Topology>,
    watching: BTreeSet<PathBuf>,
}

impl Observers {
    pub(super) fn new() -> Self {
        let wake = Arc::new(Notify::new());
        Self {
            wake,
            watcher: None,
            roots: HashMap::new(),
            watching: BTreeSet::new(),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub(super) fn retain(&mut self, wanted: &HashSet<AbsolutePath>) {
        self.roots.retain(|root, _| wanted.contains(root));
        self.update_watches();
    }

    pub(super) fn record(&mut self, root: AbsolutePath, topology: Option<Topology>) {
        if let Some(topology) = topology {
            self.roots.insert(root, topology);
        } else {
            self.roots.remove(&root);
        }
    }

    pub(super) async fn refresh(&mut self, store: &dyn WorkspaceStore) {
        let mut changed = Vec::new();
        for (root, previous) in &mut self.roots {
            if let Ok(next) = Topology::read(root.as_path()) {
                previous.directories = next.directories;
                if previous.fingerprint != next.fingerprint {
                    changed.push((root.clone(), next.fingerprint));
                }
            }
        }
        self.update_watches();
        if changed.is_empty() {
            return;
        }
        let actor = Actor::local_operator();
        let Ok(current) = store.workspace_context(actor.clone()).await else {
            return;
        };
        for (root, topology) in changed {
            match store
                .request_checkout_projection(
                    actor.clone(),
                    current.value.revision,
                    root.clone(),
                    topology,
                )
                .await
            {
                Ok(result) if result.value => {
                    if let Some(previous) = self.roots.get_mut(&root) {
                        previous.fingerprint = topology;
                    }
                }
                Ok(_) => {
                    self.roots.remove(&root);
                }
                Err(_) => (),
            }
        }
        self.update_watches();
    }

    fn update_watches(&mut self) {
        if self.roots.is_empty() {
            self.watcher = None;
            self.watching.clear();
            return;
        }
        if self.watcher.is_none() {
            self.watcher = watcher(self.wake.clone());
        }
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        let desired: BTreeSet<_> = self
            .roots
            .values()
            .flat_map(|root| root.directories.iter())
            .take(MAX_WATCH_PATHS)
            .cloned()
            .collect();
        self.watching.retain(|path| {
            if desired.contains(path) {
                true
            } else {
                let _ = watcher.unwatch(path);
                false
            }
        });
        for path in desired
            .difference(&self.watching)
            .cloned()
            .collect::<Vec<_>>()
        {
            if watcher.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                self.watching.insert(path);
            }
        }
    }
}

fn watcher(publish: Arc<Notify>) -> Option<RecommendedWatcher> {
    notify::recommended_watcher(move |event: notify::Result<Event>| match event {
        Ok(event) if matches!(event.kind, EventKind::Access(_)) => (),
        Ok(event)
            if !event.paths.iter().any(|path| {
                path.ends_with("worktrees")
                    || path.parent().is_some_and(|dir| dir.ends_with("worktrees"))
                    || path
                        .parent()
                        .and_then(Path::parent)
                        .is_some_and(|dir| dir.ends_with("worktrees"))
                        && (path.ends_with("index") || path.ends_with("gitdir"))
            }) => {}
        _ => publish.notify_one(),
    })
    .ok()
}
