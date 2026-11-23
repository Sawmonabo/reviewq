use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    Failure, Release,
    bundle::{self, Bundle, Entry},
    files,
    paths::{self, Paths},
};

pub(crate) const PREVIOUS_ASSETS: &str = "previous-assets";
const JOURNAL: &str = "release-retention.json";
const JOURNAL_LIMIT: u64 = 8 * 1024 * 1024;
const RETIRE_PREFIX: &str = ".retire-";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WebAssets {
    release: String,
    entries: Vec<Entry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StagedWeb {
    assets: WebAssets,
    staging: String,
    source: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    retained: Option<WebAssets>,
    planned: Option<StagedWeb>,
    retirement: Option<String>,
}

pub(super) struct Plan {
    journal: Journal,
}

impl Plan {
    pub(super) fn prepare(
        paths: &Paths,
        old: Option<&Release>,
        next: &Bundle,
        known: &[String],
        staging: &str,
    ) -> Result<Self, Failure> {
        let mut journal = Journal::load(paths)?;
        journal.finish_retirement(paths)?;
        if journal
            .planned
            .as_ref()
            .is_some_and(|plan| plan.staging == staging && plan.assets.release == next.id)
        {
            return Ok(Self { journal });
        }
        if journal
            .planned
            .as_ref()
            .is_some_and(|record| old.is_some_and(|release| record.assets.release == release.id))
        {
            journal.retained = journal.planned.take().map(|plan| plan.assets);
        }
        let before = old.and_then(|release| installed_bundle(paths, &release.id).ok());
        let (entries, source) = match before {
            Some(previous) if web_files(&previous).eq(web_files(next)) => {
                let preserved = journal
                    .retained
                    .as_ref()
                    .filter(|record| record.release == previous.id);
                if let Some(record) = preserved {
                    (record.entries.clone(), None)
                } else if !paths::exists(&paths.profile.join("assets"))? {
                    (Vec::new(), None)
                } else {
                    // The historical journal records known bundles, including candidates
                    // that may never have become active. Use its nearest verified web difference.
                    let preceding = known
                        .iter()
                        .rev()
                        .filter(|id| **id != next.id && **id != previous.id)
                        .filter_map(|id| installed_bundle(paths, id).ok())
                        .find(|bundle| {
                            !web_files(bundle).eq(web_files(next)) && bundle.verify().is_ok()
                        });
                    preceding.map_or_else(
                        || Ok((Vec::new(), None)),
                        |bundle| previous_assets(bundle, next),
                    )?
                }
            }
            Some(previous) => previous_assets(previous, next)?,
            None => (Vec::new(), None),
        };
        journal.planned = Some(StagedWeb {
            assets: WebAssets {
                release: next.id.clone(),
                entries,
            },
            staging: staging.to_owned(),
            source,
        });
        let plan = Self { journal };
        plan.preflight(paths)?;
        Ok(plan)
    }

    fn preflight(&self, paths: &Paths) -> Result<(), Failure> {
        let planned = self
            .journal
            .planned
            .as_ref()
            .ok_or("Missing retained web plan.")?;
        let root = paths.profile.join(PREVIOUS_ASSETS);
        if !planned.assets.entries.is_empty() && paths::exists(&root)? {
            paths::ensure_dir(&root, true)?;
        }
        for entry in &planned.assets.entries {
            let path = root.join(asset_name(entry)?);
            if paths::exists(&path)? {
                if !matches_asset(&path, &[entry]) {
                    return Err(paths::conflict(&path));
                }
            } else if let Some(source) = &planned.source {
                let source = release_path(paths, source).join(&entry.path);
                if !matches_asset(&source, &[entry]) {
                    return Err(paths::conflict(&source));
                }
            }
        }
        Ok(())
    }

    pub(super) fn record(&self, paths: &Paths) -> Result<(), Failure> {
        self.journal.save(paths)
    }

    pub(super) fn publish(&self, paths: &Paths, stage: &Path) -> Result<(), Failure> {
        let root = paths.profile.join(PREVIOUS_ASSETS);
        let planned = self
            .journal
            .planned
            .as_ref()
            .ok_or("Missing retained web plan.")?;
        for entry in &planned.assets.entries {
            let path = root.join(asset_name(entry)?);
            if paths::exists(&path)? {
                if !matches_asset(&path, &[entry]) {
                    return Err(paths::conflict(&path));
                }
                continue;
            }
            let Some(source) = &planned.source else {
                // A pre-upgrade tab can reload if its retained file was removed.
                continue;
            };
            paths::ensure_dir(paths::parent(&path)?, true)?;
            let staged = stage.join("asset");
            let source = release_path(paths, source).join(&entry.path);
            if !matches_asset(&source, &[entry]) {
                return Err(paths::conflict(&source));
            }
            files::copy(&source, &staged, entry.mode)?;
            if !matches_asset(&staged, &[entry]) {
                return Err(paths::conflict(&source));
            }
            rustix::fs::renameat_with(
                rustix::fs::CWD,
                &staged,
                rustix::fs::CWD,
                &path,
                rustix::fs::RenameFlags::NOREPLACE,
            )?;
            paths::sync(paths::parent(&path)?)?;
        }
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        paths: &Paths,
        active: &Release,
        known: &mut Vec<String>,
    ) -> Result<Vec<PathBuf>, Failure> {
        files::verify_current(paths, active)?;
        let (bundles, mut conflicts) = known_bundles(paths, known)?;
        let planned = self
            .journal
            .planned
            .take()
            .ok_or("Missing retained web plan.")?
            .assets;
        let keep = planned
            .entries
            .iter()
            .map(asset_name)
            .collect::<Result<BTreeSet<_>, _>>()?;

        prune_assets(
            &paths.profile.join("assets"),
            &bundles,
            None,
            &BTreeSet::new(),
            &mut conflicts,
        )?;
        prune_assets(
            &paths.profile.join(PREVIOUS_ASSETS),
            &bundles,
            self.journal.retained.as_ref(),
            &keep,
            &mut conflicts,
        )?;
        // Record the retained web files before their source bundle can be retired.
        self.journal.retained = (!keep.is_empty()).then_some(planned);
        self.journal.quarantine(
            paths,
            bundles.iter().filter(|bundle| bundle.id != active.id),
            &mut conflicts,
        )?;
        self.journal.save(paths)?;
        known.retain(|id| id == &active.id || conflicts.contains(&release_path(paths, id)));
        conflicts.sort_unstable();
        conflicts.dedup();
        Ok(conflicts)
    }

    pub(super) fn restore(
        mut self,
        paths: &Paths,
        active: &Release,
        failed: &Release,
        known: &mut Vec<String>,
    ) -> Result<Vec<PathBuf>, Failure> {
        files::verify_current(paths, active)?;
        let mut conflicts = Vec::new();
        let keep = self
            .journal
            .retained
            .iter()
            .flat_map(|record| &record.entries)
            .map(asset_name)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let planned = self.journal.planned.take().map(|plan| plan.assets);
        prune_assets(
            &paths.profile.join(PREVIOUS_ASSETS),
            &[],
            planned.as_ref(),
            &keep,
            &mut conflicts,
        )?;
        if failed.id != active.id {
            let directory = release_path(paths, &failed.id);
            if paths::exists(&directory)? {
                match installed_bundle(paths, &failed.id) {
                    Ok(bundle) => self.journal.quarantine(paths, [&bundle], &mut conflicts)?,
                    Err(_) => conflicts.push(directory.clone()),
                }
            }
            if !conflicts.contains(&directory) {
                known.retain(|id| id != &failed.id);
            }
        }
        self.journal.save(paths)?;
        Ok(conflicts)
    }
}

pub(super) fn remove_assets(paths: &Paths, known: &[String]) -> Result<Vec<PathBuf>, Failure> {
    let mut journal = Journal::load(paths)?;
    journal.finish_retirement(paths)?;
    journal.save(paths)?;
    let (bundles, mut conflicts) = known_bundles(paths, known)?;
    let keep = BTreeSet::new();
    prune_assets(
        &paths.profile.join("assets"),
        &bundles,
        None,
        &keep,
        &mut conflicts,
    )?;
    prune_assets(
        &paths.profile.join(PREVIOUS_ASSETS),
        &bundles,
        journal.retained.as_ref(),
        &keep,
        &mut conflicts,
    )?;
    Ok(conflicts)
}

pub(super) fn clear_assets_journal(paths: &Paths) -> Result<(), Failure> {
    remove_file(&paths.state.join(JOURNAL))
}

impl Journal {
    fn load(paths: &Paths) -> Result<Self, Failure> {
        let path = paths.state.join(JOURNAL);
        let journal =
            paths::read_json_with_limit::<Self>(&path, JOURNAL_LIMIT)?.unwrap_or_default();
        if journal
            .planned
            .as_ref()
            .and_then(|plan| plan.source.as_ref())
            .is_some_and(|id| !bundle::hash_string(id, 64))
        {
            return Err(paths::conflict(&path));
        }
        for record in journal
            .retained
            .iter()
            .chain(journal.planned.iter().map(|plan| &plan.assets))
        {
            if !bundle::hash_string(&record.release, 64) {
                return Err(paths::conflict(&path));
            }
            for entry in &record.entries {
                asset_name(entry)?;
            }
        }
        Ok(journal)
    }

    fn save(&self, paths: &Paths) -> Result<(), Failure> {
        let path = paths.state.join(JOURNAL);
        if self.retained.is_none() && self.planned.is_none() && self.retirement.is_none() {
            return remove_file(&path);
        }
        paths::write_json(&path, self)
    }

    fn quarantine<'a>(
        &mut self,
        paths: &Paths,
        bundles: impl IntoIterator<Item = &'a Bundle>,
        conflicts: &mut Vec<PathBuf>,
    ) -> Result<(), Failure> {
        let mut bundles = bundles.into_iter().peekable();
        if bundles.peek().is_none() {
            return Ok(());
        }
        let operation = files::reserve_operation(paths, RETIRE_PREFIX)?;
        let stage = files::operation_path(paths, RETIRE_PREFIX, &operation)?;
        self.retirement = Some(operation);
        self.save(paths)?;
        paths::ensure_dir(&stage, true)?;
        for bundle in bundles {
            if paths::ensure_dir(&bundle.root, true).is_ok() && bundle.verify().is_ok() {
                files::quarantine(&stage, &bundle.id, &bundle.root)?;
            } else {
                conflicts.push(bundle.root.clone());
            }
        }
        self.finish_retirement(paths)
    }

    fn finish_retirement(&mut self, paths: &Paths) -> Result<(), Failure> {
        if let Some(operation) = self.retirement.take() {
            let stage = files::operation_path(paths, RETIRE_PREFIX, &operation)?;
            if paths::exists(&stage)? {
                paths::ensure_dir(&stage, true)?;
                fs::remove_dir_all(&stage)?;
                paths::sync(paths::parent(&stage)?)?;
            }
        }
        Ok(())
    }
}

fn previous_assets(
    previous: Bundle,
    next: &Bundle,
) -> Result<(Vec<Entry>, Option<String>), Failure> {
    let active: BTreeMap<_, _> = next.assets().collect();
    let mut entries = Vec::new();
    for (name, entry) in previous.assets() {
        if !fingerprinted(name) {
            continue;
        }
        if let Some(new) = active.get(name) {
            if *new != entry {
                return Err(
                    format!("Different web assets share the fingerprinted path {name}.").into(),
                );
            }
        } else {
            entries.push(entry.clone());
        }
    }
    Ok((entries, Some(previous.id)))
}

fn asset_name(entry: &Entry) -> Result<&str, Failure> {
    entry
        .path
        .strip_prefix("web/assets/")
        .filter(|name| bundle::relative(name))
        .ok_or_else(|| "Invalid retained web asset path.".into())
}

fn fingerprinted(name: &str) -> bool {
    let Some((stem, _)) = name.rsplit_once('.') else {
        return false;
    };
    match stem.as_bytes().last_chunk::<9>() {
        Some([b'-', hash @ ..]) => hash
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(byte)),
        _ => false,
    }
}

fn web_files(bundle: &Bundle) -> impl Iterator<Item = &Entry> {
    bundle
        .manifest
        .files
        .iter()
        .filter(|entry| entry.path.starts_with("web/"))
}

fn release_path(paths: &Paths, id: &str) -> PathBuf {
    paths.profile.join("releases").join(id)
}

fn installed_bundle(paths: &Paths, id: &str) -> Result<Bundle, Failure> {
    if !bundle::hash_string(id, 64) {
        return Err("Invalid known release identity.".into());
    }
    let directory = release_path(paths, id);
    let bundle = Bundle::inventory(&directory)?;
    if bundle.id == id {
        Ok(bundle)
    } else {
        Err(paths::conflict(&directory))
    }
}

fn known_bundles(paths: &Paths, known: &[String]) -> Result<(Vec<Bundle>, Vec<PathBuf>), Failure> {
    let mut bundles = Vec::new();
    let mut conflicts = Vec::new();
    for id in known {
        if !bundle::hash_string(id, 64) {
            return Err("Invalid known release identity.".into());
        }
        let directory = release_path(paths, id);
        if paths::exists(&directory)? {
            match installed_bundle(paths, id) {
                Ok(bundle) => bundles.push(bundle),
                Err(_) => conflicts.push(directory),
            }
        }
    }
    Ok((bundles, conflicts))
}

fn prune_assets(
    root: &Path,
    bundles: &[Bundle],
    retained: Option<&WebAssets>,
    keep: &BTreeSet<&str>,
    conflicts: &mut Vec<PathBuf>,
) -> Result<(), Failure> {
    if !paths::exists(root)? {
        return Ok(());
    }
    if paths::ensure_dir(root, true).is_err() {
        conflicts.push(root.to_path_buf());
        return Ok(());
    }
    let mut owned: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for (name, entry) in bundles.iter().flat_map(Bundle::assets) {
        owned.entry(name).or_default().push(entry);
    }
    if let Some(record) = retained {
        for entry in &record.entries {
            owned.entry(asset_name(entry)?).or_default().push(entry);
        }
    }
    let mut directories = BTreeSet::new();
    for (name, entries) in owned {
        if keep.contains(name) {
            continue;
        }
        let path = root.join(name);
        let directory = paths::parent(&path)?;
        if paths::exists(directory)? {
            if paths::ensure_dir(directory, true).is_err() {
                conflicts.push(directory.to_path_buf());
                continue;
            }
            if paths::exists(&path)? {
                if !matches_asset(&path, &entries) {
                    conflicts.push(path);
                    continue;
                }
                remove_file(&path)?;
            }
            for ancestor in directory.ancestors().take_while(|path| *path != root) {
                directories.insert(ancestor.to_path_buf());
            }
        }
    }
    for directory in directories.iter().rev().map(PathBuf::as_path).chain([root]) {
        match fs::remove_dir(directory) {
            Ok(()) => paths::sync(paths::parent(directory)?)?,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn matches_asset(path: &Path, entries: &[&Entry]) -> bool {
    let check = || -> Result<bool, Failure> {
        // Reject symlinked parents as well as symlinks and hard links at the file.
        if path.canonicalize()? != path {
            return Ok(false);
        }
        let file = bundle::regular(path)?;
        paths::private(&file, path)?;
        let size = file.metadata()?.len();
        let digest = bundle::digest_file(file)?;
        Ok(entries
            .iter()
            .any(|entry| entry.size == size && entry.sha256 == digest))
    };
    check().unwrap_or(false)
}

fn remove_file(path: &Path) -> Result<(), Failure> {
    if paths::exists(path)? {
        fs::remove_file(path)?;
        paths::sync(paths::parent(path)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn identical_update_adopts_one_legacy_web_build_and_keeps_it_on_retry() -> Result<(), Failure> {
        let home = tempfile::tempdir()?;
        let home = home.path().canonicalize()?;
        let paths = Paths {
            profile: home.join("profile"),
            config_home: home.join("config"),
            state_home: home.join("state"),
            config: home.join("config/reviewq"),
            state: home.join("state/reviewq"),
            launcher: home.join("bin/reviewq"),
            unit: home.join("config/systemd/user/reviewq.service"),
            codex_home: home.join("codex"),
            claude_home: home.join("claude"),
            claude_home_is_explicit: false,
            home,
        };
        paths.prepare()?;
        let (archived, archived_asset) = fixture(&paths, "a", "old-abcdef12.js")?;
        let (previous, previous_asset) = fixture(&paths, "b", "new-abcdef12.js")?;
        let (next, _) = fixture(&paths, "c", "new-abcdef12.js")?;
        let previous_release = release(&previous);
        let next_release = release(&next);
        let unknown = paths.profile.join("releases/notes");
        paths::ensure_dir(&unknown, true)?;
        let legacy = paths.profile.join("assets");
        paths::ensure_dir(&legacy, true)?;
        for (source, entry) in [(&archived, &archived_asset), (&previous, &previous_asset)] {
            files::copy(
                &source.root.join(&entry.path),
                &legacy.join(asset_name(entry)?),
                entry.mode,
            )?;
        }
        let user_file = legacy.join("notes.txt");
        paths::atomic_file(&user_file, b"user notes", false)?;
        let mut known = vec![archived.id.clone(), previous.id.clone(), next.id.clone()];
        symlink(
            Path::new("releases").join(&previous.id),
            paths.profile.join("current"),
        )?;
        let stage = paths.profile.join("releases/test-stage");
        paths::ensure_dir(&stage, true)?;
        let planned = Plan::prepare(&paths, Some(&previous_release), &next, &known, "test")?;
        planned.record(&paths)?;

        // Retry after recording intent but before copying the older lazy chunk.
        let planned = Plan::prepare(&paths, Some(&previous_release), &next, &known, "test")?;
        planned.publish(&paths, &stage)?;
        fs::remove_file(paths.profile.join("current"))?;
        symlink(
            Path::new("releases").join(&next.id),
            paths.profile.join("current"),
        )?;
        planned.finish(&paths, &next_release, &mut known)?;
        assert_eq!(known, [next.id.clone()]);
        assert!(!archived.root.exists() && !previous.root.exists());
        assert!(next.root.is_dir() && unknown.is_dir());
        assert!(user_file.is_file());
        assert!(!legacy.join(asset_name(&previous_asset)?).exists());
        let compatible = paths.profile.join(PREVIOUS_ASSETS);
        assert!(matches_asset(
            &compatible.join(asset_name(&archived_asset)?),
            &[&archived_asset]
        ));
        assert!(!compatible.join(asset_name(&previous_asset)?).exists());

        let repeated = Plan::prepare(&paths, Some(&next_release), &next, &known, "repeat")?;
        repeated.record(&paths)?;
        repeated.publish(&paths, &stage)?;
        repeated.finish(&paths, &next_release, &mut known)?;
        assert!(matches_asset(
            &compatible.join(asset_name(&archived_asset)?),
            &[&archived_asset]
        ));
        Ok(())
    }

    fn fixture(paths: &Paths, label: &str, chunk: &str) -> Result<(Bundle, Entry), Failure> {
        let path = format!("web/assets/{chunk}");
        let entry = Entry {
            path,
            mode: 0o600,
            size: chunk.len() as u64,
            sha256: bundle::hex(&Sha256::digest(chunk.as_bytes())),
        };
        // These historical manifests deliberately omit components now required for install.
        let files = serde_json::to_string(&[&entry])?;
        let manifest = format!(
            "{{\"format\":1,\"version\":\"{label}\",\"target\":\"legacy\",\"source\":\"legacy\",\"protocol\":1,\"store\":1,\"files\":{files}}}\n"
        );
        let id = bundle::hex(&Sha256::digest(manifest.as_bytes()));
        let root = release_path(paths, &id);
        let file = root.join(&entry.path);
        paths::ensure_dir(paths::parent(&file)?, true)?;
        paths::atomic_file(&file, chunk.as_bytes(), false)?;
        paths::atomic_file(&root.join("manifest.json"), manifest.as_bytes(), false)?;
        Ok((installed_bundle(paths, &id)?, entry))
    }

    fn release(bundle: &Bundle) -> Release {
        Release {
            id: bundle.id.clone(),
            binary: String::new(),
            unit: String::new(),
            store: bundle.manifest.store,
            skill: None,
            claude: None,
            windows: None,
        }
    }
}
