use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::AppError;
use reviewq_app::history::{HistoryParent, HistorySession};
use reviewq_app::hosts::HostProof;
use reviewq_domain::identity::NativeSessionId;
use reviewq_domain::session::{AbsolutePath, HostProvider, NativeBinding, NativeIdentity};
use rustix::fs::{self as secure, Mode, OFlags, ResolveFlags};
use sha2::{Digest, Sha256};

use super::{file, identity};

const MAX_DISCOVERY_ENTRIES: usize = 16_384;
const IDENTITY_SCAN_BYTES: u64 = 64 * 1024 * 1024;

pub(super) struct Source {
    pub file: File,
    pub provider: HostProvider,
    pub native_id: NativeSessionId,
    pub fingerprint: [u8; 32],
    pub root_session: Option<NativeSessionId>,
    pub(super) path: PathBuf,
}

pub(crate) async fn verified_codex_source(
    binding: NativeBinding,
    path: PathBuf,
) -> Result<Option<AbsolutePath>, AppError> {
    tokio::task::spawn_blocking(move || {
        let cancelled = AtomicBool::new(false);
        if verify(&binding, None, &path, &cancelled)?.is_none() {
            return Ok(None);
        }
        AbsolutePath::from_canonical(path)
            .map(Some)
            .map_err(|_| AppError::Forbidden)
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

pub(super) fn locate(
    session: &HistorySession,
    cancelled: &AtomicBool,
) -> Result<Option<Source>, AppError> {
    if let Some(path) = &session.source {
        return verify(
            &session.binding,
            session.parent.as_ref(),
            path.as_path(),
            cancelled,
        )?
        .ok_or(AppError::NotFound)
        .map(Some);
    }
    discover(&session.binding, session.parent.as_ref(), cancelled)
}

pub(crate) fn verified_claude_source(
    binding: &NativeBinding,
    path: Option<&AbsolutePath>,
    cancelled: &AtomicBool,
) -> Result<Option<AbsolutePath>, AppError> {
    let source = match path {
        Some(path) => verify(binding, None, path.as_path(), cancelled)?,
        None => discover(binding, None, cancelled)?,
    };
    source
        .map(|source| AbsolutePath::from_canonical(source.path).map_err(|_| AppError::Forbidden))
        .transpose()
}

/// Reads the exact user's requested lead UUID in this profile. The cwd comes from
/// the native row, since the configured review checkout is a separate choice.
pub(crate) fn inspect_claude_lead(
    profile: AbsolutePath,
    native: NativeSessionId,
    cancelled: &AtomicBool,
) -> Result<HostProof, AppError> {
    let identity = NativeIdentity::new(HostProvider::Claude, profile, native);
    let profile_root = identity.profile().as_path();
    let projects = profile_root.join("projects");
    let directory = match fs::symlink_metadata(&projects) {
        Ok(metadata) if metadata.is_dir() => fs::read_dir(&projects).map_err(fs_error)?,
        Ok(_) => return Err(AppError::Forbidden),
        Err(error) => return Err(fs_error(error)),
    };
    let filename = format!("{}.jsonl", identity.native_id());
    let mut found = None;
    for (index, directory) in directory.enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(AppError::Unavailable);
        }
        if index >= MAX_DISCOVERY_ENTRIES {
            return Err(AppError::Busy);
        }
        let directory = directory.map_err(fs_error)?;
        if !directory.file_type().map_err(fs_error)?.is_dir() {
            continue;
        }
        let candidate = directory.path().join(&filename);
        let rel = candidate
            .strip_prefix(profile_root)
            .map_err(|_| AppError::Forbidden)?;
        let Some(file) = source_file(profile_root, rel)? else {
            continue;
        };
        let length = file.metadata().map_err(fs_error)?.len();
        let mut budget = IDENTITY_SCAN_BYTES;
        let mut offset = 0;
        for _ in 0..64 {
            if offset == length {
                break;
            }
            let end = file::next_line(&file, offset, length, &mut budget, cancelled)?;
            let record = identity::read(
                HostProvider::Claude,
                file::input(&file, offset, end, cancelled),
            );
            offset = end;
            let Ok(record) = record else {
                continue;
            };
            if record.sidechain || record.session.as_deref() != Some(identity.native_id().as_str())
            {
                continue;
            }
            let Some(cwd) = record.cwd.filter(|cwd| cwd.is_absolute()) else {
                continue;
            };
            let cwd = cwd.canonicalize().map_err(fs_error)?;
            if !cwd.is_dir() {
                return Err(AppError::NotFound);
            }
            let binding = NativeBinding::new(
                identity.clone(),
                AbsolutePath::from_canonical(cwd).map_err(|_| AppError::Forbidden)?,
            );
            let source = verified_claude_source(
                &binding,
                Some(&AbsolutePath::from_canonical(candidate).map_err(|_| AppError::Forbidden)?),
                cancelled,
            )?
            .ok_or(AppError::Conflict)?;
            if found.is_some() {
                return Err(AppError::Conflict);
            }
            found = Some(HostProof {
                binding,
                source: Some(source),
                activity: None,
            });
            break;
        }
    }
    found.ok_or(AppError::NotFound)
}

fn discover(
    binding: &NativeBinding,
    child: Option<&HistoryParent>,
    cancelled: &AtomicBool,
) -> Result<Option<Source>, AppError> {
    let identity = binding.identity();
    let profile = identity.profile().as_path();
    let native = identity.native_id().as_str();
    let (roots, depth, suffix) = match identity.provider() {
        HostProvider::Codex => (
            vec![profile.join("sessions"), profile.join("archived_sessions")],
            3,
            format!("-{native}.jsonl"),
        ),
        HostProvider::Claude if child.is_none() => {
            (vec![profile.join("projects")], 1, format!("{native}.jsonl"))
        }
        HostProvider::Claude => return Ok(None),
    };
    let mut budget = MAX_DISCOVERY_ENTRIES;
    let mut match_source = None;
    for root in roots {
        visit(&root, depth, &mut budget, cancelled, &mut |path| {
            let name = path
                .file_name()
                .and_then(|part| part.to_str())
                .unwrap_or_default();
            let named = match identity.provider() {
                HostProvider::Codex => name.starts_with("rollout-") && name.ends_with(&suffix),
                HostProvider::Claude => name == suffix,
            };
            if named && let Some(source) = verify(binding, child, path, cancelled)? {
                if match_source.is_some() {
                    return Err(AppError::Conflict);
                }
                match_source = Some(source);
            }
            Ok(())
        })?;
    }
    Ok(match_source)
}

fn visit(
    path: &Path,
    depth: usize,
    budget: &mut usize,
    cancelled: &AtomicBool,
    observe: &mut impl FnMut(&Path) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let directory = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::read_dir(path).map_err(fs_error)?,
        Ok(_) => return Err(AppError::Forbidden),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(fs_error(error)),
    };
    for item in directory {
        if cancelled.load(Ordering::Relaxed) {
            return Err(AppError::Unavailable);
        }
        *budget = budget.checked_sub(1).ok_or(AppError::Busy)?;
        let item = item.map_err(fs_error)?;
        let class = item.file_type().map_err(fs_error)?;
        if depth > 0 && class.is_dir() {
            visit(&item.path(), depth - 1, budget, cancelled, observe)?;
        } else if class.is_file() {
            observe(&item.path())?;
        }
    }
    Ok(())
}

pub(super) fn verify(
    binding: &NativeBinding,
    parent: Option<&HistoryParent>,
    candidate: &Path,
    cancelled: &AtomicBool,
) -> Result<Option<Source>, AppError> {
    let identity = binding.identity();
    let root = identity.profile().as_path();
    let rel = candidate
        .strip_prefix(root)
        .map_err(|_| AppError::Forbidden)?;
    let mut components = rel.components();
    let first = components.next();
    let expected_folder = match identity.provider() {
        HostProvider::Codex => {
            matches!(first, Some(Component::Normal(name)) if name == "sessions" || name == "archived_sessions")
        }
        HostProvider::Claude => {
            matches!(first, Some(Component::Normal(name)) if name == "projects")
        }
    };
    if !expected_folder
        || !components
            .clone()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(AppError::Forbidden);
    }
    if identity.provider() == HostProvider::Claude
        && let Some(parent) = parent
        && !claude_child_location(
            candidate,
            claude_root(parent).as_str(),
            identity.native_id().as_str(),
        )
    {
        return Err(AppError::Forbidden);
    }
    if identity.provider() == HostProvider::Claude
        && parent.is_none()
        && (components.count() != 2
            || candidate.file_name().and_then(|name| name.to_str())
                != Some(&format!("{}.jsonl", identity.native_id())))
    {
        return Err(AppError::Forbidden);
    }
    let Some(file) = source_file(root, rel)? else {
        return Ok(None);
    };
    let metadata = file.metadata().map_err(fs_error)?;
    let provider = identity.provider();
    let mut budget = IDENTITY_SCAN_BYTES;
    let mut offset = 0;
    let header_records = if provider == HostProvider::Codex {
        1
    } else {
        64
    };
    for _ in 0..header_records {
        if offset == metadata.len() {
            break;
        }
        let end = file::next_line(&file, offset, metadata.len(), &mut budget, cancelled)?;
        let record = identity::read(provider, file::input(&file, offset, end, cancelled));
        if let Ok(record) = record
            && identity_matches(&record, binding, parent)
        {
            let mut sample = Vec::new();
            file::input(&file, offset, end, cancelled)
                .take(4096)
                .read_to_end(&mut sample)
                .map_err(fs_error)?;
            let mut fingerprint = Sha256::new();
            fingerprint.update(candidate.as_os_str().as_encoded_bytes());
            fingerprint.update(offset.to_be_bytes());
            fingerprint.update(end.to_be_bytes());
            fingerprint.update(sample);
            return Ok(Some(Source {
                file,
                provider,
                native_id: identity.native_id().clone(),
                fingerprint: fingerprint.finalize().into(),
                root_session: if provider == HostProvider::Claude {
                    parent.map(|parent| claude_root(parent).clone())
                } else {
                    None
                },
                path: candidate.to_path_buf(),
            }));
        }
        offset = end;
    }
    Ok(None)
}

fn source_file(root: &Path, rel: &Path) -> Result<Option<File>, AppError> {
    let root = secure::openat2(
        secure::CWD,
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .map(File::from)
    .map_err(|error| fs_error(error.into()))?;
    let file = match secure::openat2(
        &root,
        rel,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    ) {
        Ok(fd) => File::from(fd),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(fs_error(error.into())),
    };
    let owner = rustix::process::getuid().as_raw();
    let metadata = file.metadata().map_err(fs_error)?;
    if !metadata.is_file()
        || metadata.uid() != owner
        || root.metadata().map_err(fs_error)?.uid() != owner
    {
        return Err(AppError::Forbidden);
    }
    Ok(Some(file))
}

fn identity_matches(
    record: &identity::IdentityRecord,
    binding: &NativeBinding,
    parent: Option<&HistoryParent>,
) -> bool {
    let identity = binding.identity();
    let id = identity.native_id().as_str();
    match identity.provider() {
        HostProvider::Codex => {
            let expected = parent.map(|parent| parent.native_id.as_str());
            record.session.as_deref() == Some(id)
                && record.parent.as_deref() == expected
                && record.cwd.as_deref().is_some_and(|path| {
                    path == binding.cwd().as_path()
                        || path.canonicalize().ok().as_deref() == Some(binding.cwd().as_path())
                })
        }
        HostProvider::Claude => match parent {
            Some(parent) => {
                record.sidechain
                    && record.agent.as_deref() == Some(id)
                    && record.session.as_deref() == Some(claude_root(parent).as_str())
            }
            None => {
                !record.sidechain
                    && record.session.as_deref() == Some(id)
                    && record.cwd.as_deref().is_some_and(|path| {
                        path == binding.cwd().as_path()
                            || path.canonicalize().ok().as_deref() == Some(binding.cwd().as_path())
                    })
            }
        },
    }
}

fn claude_root(parent: &HistoryParent) -> &NativeSessionId {
    parent.root_native_id.as_ref().unwrap_or(&parent.native_id)
}

pub(crate) fn verified_claude_child(
    root: &HistorySession,
    child: &NativeBinding,
    cancelled: &AtomicBool,
) -> Result<AbsolutePath, AppError> {
    let parent = root.binding.identity();
    let child_identity = child.identity();
    if root.parent.is_some()
        || parent.provider() != HostProvider::Claude
        || child_identity.provider() != HostProvider::Claude
        || parent.profile() != child_identity.profile()
        || parent.native_id() == child_identity.native_id()
    {
        return Err(AppError::Forbidden);
    }
    let id = child_identity.native_id().as_str();
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(AppError::InvalidRequest);
    }
    let path = root.source.as_ref().ok_or(AppError::NotFound)?.as_path();
    let candidate = path
        .with_extension("")
        .join("subagents")
        .join(format!("agent-{id}.jsonl"));
    let relation = HistoryParent {
        session_key: root.target.session_key().clone(),
        native_id: parent.native_id().clone(),
        root_native_id: None,
        source: root.source.clone(),
    };
    let source =
        verify(child, Some(&relation), &candidate, cancelled)?.ok_or(AppError::NotFound)?;
    AbsolutePath::from_canonical(source.path).map_err(|_| AppError::Forbidden)
}

fn claude_child_location(path: &Path, parent: &str, id: &str) -> bool {
    path.file_name().and_then(|part| part.to_str()) == Some(&format!("agent-{id}.jsonl"))
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "subagents")
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == parent)
}

fn fs_error(error: io::Error) -> AppError {
    match error.raw_os_error() {
        Some(18 | 40) => AppError::Forbidden,
        _ => match error.kind() {
            io::ErrorKind::NotFound => AppError::NotFound,
            io::ErrorKind::PermissionDenied => AppError::Forbidden,
            _ => AppError::Unavailable,
        },
    }
}
