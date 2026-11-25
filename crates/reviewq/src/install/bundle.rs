use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Failure;

const MAX_MANIFEST: u64 = 4 * 1024 * 1024;
const MAX_FILES: usize = 16_384;
const REQUIRED: &[&str] = &[
    "bin/reviewq",
    "web/index.html",
    "web/icon.svg",
    "hosts/claude/.claude-plugin/marketplace.json",
    "hosts/claude/plugin/.claude-plugin/plugin.json",
    "hosts/claude/plugin/hooks/hooks.json",
    "hosts/claude/plugin/hooks/bridge.js",
    "hosts/claude/plugin/hooks/children.js",
    "hosts/claude/plugin/hooks/delivery.js",
    "hosts/claude/plugin/hooks/ledger.js",
    "hosts/claude/plugin/hooks/ledger-schema.js",
    "hosts/claude/plugin/hooks/questions.js",
    "hosts/claude/plugin/hooks/register.jsx",
    "hosts/claude/plugin/hooks/settings.js",
    "hosts/claude/plugin/hooks/workflow.js",
    "hosts/codex/skills/reviewq/SKILL.md",
    "hosts/codex/skills/reviewq/agents/openai.yaml",
    "hosts/shared/skills/reviewq-manage/SKILL.md",
    "packaging/reviewq.service.in",
    "packaging/notification.png",
    "licenses/DEPENDENCIES.json",
];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    format: u32,
    pub version: String,
    target: String,
    source: String,
    protocol: u32,
    pub store: i64,
    pub files: Vec<Entry>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub path: String,
    pub mode: u32,
    pub size: u64,
    pub sha256: String,
}

pub(super) struct Bundle {
    pub id: String,
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl Bundle {
    pub(super) fn read(path: &Path) -> Result<Self, Failure> {
        let bundle = Self::inventory(path)?;
        bundle.verify_supported()?;
        bundle.verify()?;
        Ok(bundle)
    }

    pub(super) fn inventory(path: &Path) -> Result<Self, Failure> {
        if !path.is_absolute() || !path.symlink_metadata()?.is_dir() || path.canonicalize()? != path
        {
            return Err("Use the canonical absolute directory of the release bundle.".into());
        }
        let mut bytes = Vec::new();
        regular(&path.join("manifest.json"))?
            .take(MAX_MANIFEST + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST {
            return Err("Release manifest exceeds 4 MiB.".into());
        }
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        let mut canonical = serde_json::to_vec(&manifest)?;
        canonical.push(b'\n');
        if canonical != bytes || manifest.format != 1 || manifest.files.len() > MAX_FILES {
            return Err("Noncanonical or unsupported release manifest.".into());
        }
        let mut previous = "";
        for entry in &manifest.files {
            if previous >= entry.path.as_str()
                || !relative(&entry.path)
                || !hash_string(&entry.sha256, 64)
                || entry.mode > 0o777
            {
                return Err(format!("Invalid release entry {}", entry.path).into());
            }
            previous = &entry.path;
        }
        Ok(Self {
            id: hex(&Sha256::digest(&bytes)),
            root: path.to_path_buf(),
            manifest,
        })
    }

    fn verify_supported(&self) -> Result<(), Failure> {
        let target = format!("{}-unknown-linux-{}", std::env::consts::ARCH, target_env());
        let manifest = &self.manifest;
        if manifest.protocol != 1
            || manifest.store != 2
            || manifest.target != target
            || !path_token(&manifest.version)
            || !hash_string(&manifest.source, 40)
        {
            return Err("Noncanonical or unsupported release manifest.".into());
        }
        for entry in &manifest.files {
            if !allowed(&entry.path)
                || entry.mode
                    != if entry.path == "bin/reviewq" {
                        0o700
                    } else {
                        0o600
                    }
            {
                return Err(format!("Invalid release entry {}", entry.path).into());
            }
        }
        if REQUIRED.iter().any(|path| {
            manifest
                .files
                .binary_search_by_key(path, |entry| entry.path.as_str())
                .is_err()
        }) {
            return Err("A required release component is missing.".into());
        }
        Ok(())
    }

    pub(super) fn verify(&self) -> Result<(), Failure> {
        let mut actual = BTreeSet::new();
        walk(&self.root, &self.root, &mut actual)?;
        let mut expected = BTreeSet::from(["manifest.json".to_owned()]);
        for entry in &self.manifest.files {
            let file = regular(&self.root.join(&entry.path))?;
            let info = file.metadata()?;
            if info.len() != entry.size
                || info.permissions().mode() & 0o777 != entry.mode
                || digest_file(file)? != entry.sha256
            {
                return Err(format!("Release contents changed at {}", entry.path).into());
            }
            expected.insert(entry.path.clone());
        }
        if expected != actual {
            return Err("Release inventory differs from the files on disk.".into());
        }
        Ok(())
    }

    pub(super) fn assets(&self) -> impl Iterator<Item = (&str, &Entry)> {
        self.manifest
            .files
            .iter()
            .filter_map(|entry| Some((entry.path.strip_prefix("web/assets/")?, entry)))
    }
}

fn walk(root: &Path, directory: &Path, output: &mut BTreeSet<String>) -> Result<(), Failure> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = path
            .strip_prefix(root)?
            .to_str()
            .ok_or("Non-UTF-8 bundle path.")?;
        if !relative(name) {
            return Err(format!("Unsupported release path {name}").into());
        }
        match entry.file_type()? {
            kind if kind.is_dir() => {
                let before = output.len();
                walk(root, &path, output)?;
                if output.len() == before {
                    return Err(format!("Empty release directory {name}").into());
                }
            }
            kind if kind.is_file() => {
                output.insert(name.to_owned());
                if output.len() > MAX_FILES + 1 {
                    return Err("Release directory contains too many files.".into());
                }
            }
            _ => return Err(format!("Nonregular release entry {name}").into()),
        }
    }
    Ok(())
}

pub(super) fn relative(value: &str) -> bool {
    value.len() <= 1024
        && value
            .split('/')
            .all(|part| path_token(part) && part != "." && part != "..")
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn path_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._@+-".contains(&c))
}

fn allowed(path: &str) -> bool {
    path == "bin/reviewq"
        || matches!(
            path,
            "packaging/reviewq.service.in" | "packaging/notification.png"
        )
        || ["web/", "hosts/", "licenses/"]
            .iter()
            .any(|prefix| path.starts_with(prefix))
}

pub(super) fn hash_string(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
}

const fn target_env() -> &'static str {
    if cfg!(target_env = "musl") {
        "musl"
    } else {
        "gnu"
    }
}

pub(super) fn regular(path: &Path) -> Result<File, Failure> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    let info = file.metadata()?;
    if !info.is_file() || info.nlink() != 1 {
        return Err(format!("Expected a regular file at {}", path.display()).into());
    }
    Ok(file)
}

pub(super) fn digest_file(mut file: File) -> io::Result<String> {
    let mut digest = Sha256::new();
    let mut chunk = [0; 32 * 1024];
    loop {
        let consumed = file.read(&mut chunk)?;
        if consumed == 0 {
            break;
        }
        digest.update(&chunk[..consumed]);
    }
    Ok(hex(&digest.finalize()))
}

pub(super) fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").expect("writing to String");
    }
    result
}
