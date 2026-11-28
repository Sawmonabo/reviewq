use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reviewq_app::access::Actor;
use reviewq_app::hosts::{ClaudeCompanion, LeadStore, NativeHost, recover_delivery};
use reviewq_app::launch::{
    LaunchStore, NativeLauncher, NativeLaunchers, NativeModelReader, NativeModels,
};
use reviewq_app::notifications::DesktopStore;
use reviewq_app::reports::DrawingRenderer;
use reviewq_app::settings::{NativeSkillReader, NativeSkills, RuntimeSources};
use reviewq_app::sources::{ReviewSourceReader, SourceRefreshStore};
use reviewq_app::workspace::{WorkspaceConfig, WorkspaceStore};
use reviewq_domain::session::{AbsolutePath, HostProvider};
use reviewq_providers::{
    Claude, ClaudeCli, ClaudeWorkflow, Codex, FileHistory, GitLab, SourceAccounts, SourceNetwork,
    Terminal, TerminalProvider, Workflows,
};
use reviewq_store::SqliteStore;
use reviewq_transport::{
    BrowserDesktop, ServiceEndpoint, ServiceIdentity, StartupState, WorkflowIntegration,
};
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::{cli::Serve, install, runtime_settings};

mod desktop;
mod diagram;
mod http;
mod workflow;

const RUNTIME: &str = "service.json";
const MAX_DESCRIPTOR: u64 = 8192;
type Failure = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn has_descriptor(profile: &Path) -> Result<bool, std::io::Error> {
    match fs::symlink_metadata(profile.join(RUNTIME)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(crate) fn profile(override_path: Option<PathBuf>) -> Result<PathBuf, Failure> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    let xdg = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    Ok(xdg
        .or_else(|| home.map(|path| path.join(".local/share")))
        .ok_or("cannot resolve a private profile")?
        .join("reviewq"))
}

pub(crate) async fn serve(profile: PathBuf, options: Serve) -> Result<(), Failure> {
    let settings = runtime_settings::Settings::load(&profile, options.validation)?;
    let (browser_port, native_port) = settings.ports(options.port, options.native_port)?;
    let web = match options.web.as_ref() {
        Some(path) => path.clone(),
        None if options.validation => {
            return Err("validation requires an explicit web asset directory".into());
        }
        None => profile.join("current/web"),
    }
    .canonicalize()?;
    let retained_assets = profile.join(install::PREVIOUS_ASSETS);
    let retained_assets = (options.web.is_none()
        && retained_assets
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir()))
    .then_some(retained_assets);
    if !web.join("index.html").is_file() {
        return Err("build the browser assets before serving".into());
    }
    if options.validation && profile == self::profile(None)? {
        return Err("validation requires a separate private runtime".into());
    }
    create_profile(&profile)?;
    let build_digest = executable_digest()?;
    let managed = if options.managed {
        Some(install::Managed::verify(&profile, &build_digest).await?)
    } else {
        if self::profile(None)
            .as_ref()
            .is_ok_and(|installed| *installed == profile)
            && install::installed(&install::paths::Paths::resolve()?)?.is_some()
        {
            return Err("Use reviewq start for the installed profile.".into());
        }
        None
    };
    let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, browser_port)).await?;
    let v6 = match native_listener(native_port) {
        Ok(listener) => Some(listener),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
            ) || error.raw_os_error() == Some(rustix::io::Errno::AFNOSUPPORT.raw_os_error()) =>
        {
            None
        }
        Err(error) => return Err(error.into()),
    };
    let browser = v4.local_addr()?;
    let native = v6
        .as_ref()
        .map(TcpListener::local_addr)
        .transpose()?
        .unwrap_or(browser);
    let store = Arc::new(
        SqliteStore::open(AbsolutePath::from_canonical(
            profile.join("reviewq.sqlite3"),
        )?)
        .await?,
    );
    let identity = store.identity();
    let store_id = identity.store_id.to_string();
    if let Some(managed) = &managed {
        managed.check_store(&store_id, identity.format)?;
    }
    let endpoint = ServiceEndpoint::new(
        browser,
        native,
        ServiceIdentity {
            build_digest,
            executable_file: executable_file()?,
            profile_id: profile_id(&profile),
            store_id: store_id.clone(),
            store_application_id: identity.application_id,
            store_schema: identity.format,
        },
    )?;
    let network = Arc::new(SourceNetwork::new()?);
    let settings_sources = Arc::new(SourceAccounts::new(
        network.clone(),
        profile.clone(),
        options.validation,
    ));
    let source = match options.gitlab.as_ref() {
        Some(origin) => {
            let url = reqwest::Url::parse(origin)?;
            if !url
                .host_str()
                .and_then(|host| {
                    host.trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .ok()
                })
                .is_some_and(|ip| ip.is_loopback())
            {
                return Err("validation sources must be loopback".into());
            }
            Some(GitLab::new(origin, None, profile.clone(), network.clone())?)
        }
        None => None,
    };
    if let Some(source) = source.as_ref() {
        let actor = Actor::local_operator();
        let current = store.workspace_context(actor.clone()).await?.value;
        let mut projects = options
            .projects
            .iter()
            .map(|id| source.configured_project(*id))
            .collect::<Vec<_>>();
        projects.sort_by_key(|project| project.project_id);
        projects.dedup();
        if current.projects != projects {
            store
                .replace_workspace(
                    actor,
                    current.revision,
                    WorkspaceConfig {
                        projects,
                        grants: Vec::new(),
                    },
                )
                .await?;
        }
    }
    let validation_source = source.map(|source| Arc::new(source) as Arc<dyn ReviewSourceReader>);
    let source = Arc::new(RuntimeSources::new(
        store.clone(),
        settings_sources.clone(),
        validation_source,
    ));
    let codex_executable = options.codex_bin.or_else(|| {
        default_executable(
            "codex",
            settings.codex_command.as_deref(),
            options.validation,
        )
    });
    let codex_home = options.codex_home.or_else(|| {
        (!options.validation)
            .then(|| chosen_profile("CODEX_HOME", ".codex"))
            .flatten()
            .filter(|home| home.is_dir())
    });
    // Only the installed service trusts the Codex hooks the installer wrote.
    let installed = options
        .managed
        .then(install::paths::Paths::resolve)
        .and_then(Result::ok)
        .filter(|paths| paths.profile == profile);
    let codex = match (codex_executable.as_ref(), codex_home.as_ref()) {
        (Some(executable), Some(home)) => {
            let metadata = home.canonicalize()?.metadata()?;
            if metadata.uid() != rustix::process::getuid().as_raw() {
                return Err("the selected Codex profile must belong to the service user".into());
            }
            let settings = options.codex_model.zip(options.codex_effort);
            let hook_command = installed.as_ref().and_then(install::codex_hook_command);
            Some(Arc::new(
                Codex::new(executable.clone(), home.clone(), settings)?
                    .with_hook_command(hook_command),
            ))
        }
        _ => None,
    };
    let host = codex.clone().map(|host| host as Arc<dyn NativeHost>);
    let checkout_claude_profile = options.claude_home.clone();
    let claude_home_is_explicit =
        options.claude_home.is_some() || std::env::var_os("CLAUDE_CONFIG_DIR").is_some();
    let claude_home = match options.claude_home {
        Some(home) => Some(home),
        None if !options.validation => claude_home().ok().filter(|path| path.is_dir()),
        None => None,
    };
    let claude = claude_home
        .clone()
        .map(Claude::new)
        .transpose()?
        .map(Arc::new);
    let claude_workflow = claude.as_ref().map(|native| {
        Arc::new(ClaudeWorkflow::new(native.clone(), store.clone()))
            as Arc<dyn reviewq_app::workflow::NativeWorkflowProof>
    });
    let workflow = Arc::new(Workflows::new(codex.clone()).with_claude(claude_workflow));
    let sends_allowed = !options.validation && (host.is_some() || claude.is_some());
    let claude = claude.map(|native| native as Arc<dyn ClaudeCompanion>);
    let claude_executable = options.claude_bin.or_else(|| {
        default_executable(
            "claude",
            settings.claude_command.as_deref(),
            options.validation,
        )
    });
    let claude_cli = claude_executable
        .clone()
        .zip(claude_home.clone())
        .map(|(executable, home)| ClaudeCli::new(executable, home, claude_home_is_explicit, None))
        .transpose()?
        .map(Arc::new);
    let skills = NativeSkills {
        codex: codex.clone().map(|host| host as Arc<dyn NativeSkillReader>),
        claude: claude_cli
            .clone()
            .map(|host| host as Arc<dyn NativeSkillReader>),
    };
    let models = NativeModels {
        codex: codex.clone().map(|host| host as Arc<dyn NativeModelReader>),
        claude: claude_cli
            .clone()
            .map(|host| host as Arc<dyn NativeModelReader>),
    };
    let claude_interrupt = claude_cli
        .clone()
        .map(|cli| cli as Arc<dyn reviewq_app::hosts::recovery::NativeJobInterrupt>);
    let launchers = NativeLaunchers {
        codex: codex.map(|host| host as Arc<dyn NativeLauncher>),
        claude: claude_cli.map(|host| host as Arc<dyn NativeLauncher>),
    };
    let terminal_provider =
        |executable: Option<PathBuf>, profile: Option<PathBuf>, profile_is_explicit| {
            executable
                .zip(profile)
                .map(|(executable, profile)| TerminalProvider {
                    executable,
                    profile,
                    profile_is_explicit,
                })
        };
    let distribution = if options.validation {
        None
    } else {
        options
            .wsl_distribution
            .or(settings.wsl_distribution)
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var("WSL_DISTRO_NAME").ok())
    };
    let terminal = Arc::new(Terminal::new(
        terminal_provider(codex_executable, codex_home, true),
        terminal_provider(claude_executable, claude_home, claude_home_is_explicit),
        distribution,
    ));
    let (shutdown, wait) = watch::channel(false);
    let (activate, startup) = watch::channel(StartupState::Verifying);
    let browser_desktop =
        (!options.validation && settings.desktop_notifications).then(BrowserDesktop::default);
    let windows = managed
        .as_ref()
        .map(install::Managed::windows_sender)
        .transpose()
        .map(Option::flatten);
    let chromium = settings
        .chromium_command
        .as_deref()
        .and_then(runtime_settings::executable);
    let drawing_renderer = {
        let (web, profile) = (web.clone(), profile.clone());
        tokio::task::spawn_blocking(move || diagram::Chromium::open(chromium, web, profile))
            .await
            .ok()
            .flatten()
            .map(|browser| Arc::new(browser) as Arc<dyn DrawingRenderer>)
    };
    let router = reviewq_transport::router(
        store.clone(),
        source.clone(),
        host.clone(),
        Arc::new(
            FileHistory::with_search_profile(profile.clone())
                .with_search_index(store.history_search_index()),
        ),
        reviewq_transport::ServiceRuntime {
            sends_allowed,
            source_poll_interval: (!options.validation)
                .then_some(Duration::from_secs(settings.poll_seconds)),
            settings_sources,
            skills,
            models,
            launchers,
            launches_allowed: !options.validation,
            claude,
            claude_interrupt,
            terminal,
            browser_desktop: browser_desktop.clone(),
            workflow: WorkflowIntegration {
                proof: workflow.clone(),
            },
            endpoint: endpoint.clone(),
            web: &web,
            retained_assets: retained_assets.as_deref(),
            drawing_renderer,
            commits: store.subscribe(),
            shutdown: wait.clone(),
            startup,
        },
    )?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    if options.managed {
        install::bridge::publish(&endpoint)?;
    }
    publish(&profile, &endpoint)?;
    let stop_signal = shutdown.clone();
    let stop = tokio::spawn(async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        stop_signal.send_replace(true);
    });
    let mut listeners = tokio::task::JoinSet::new();
    listeners.spawn(http::serve(v4, router.clone(), wait.clone()));
    if let Some(v6) = v6 {
        listeners.spawn(http::serve(v6, router, wait.clone()));
    }
    let start = async {
        if let Some(managed) = &managed
            && !managed
                .wait(&store_id, install::StartupPermission::Recover, wait.clone())
                .await?
        {
            return Ok(false);
        }
        store.recover_refresh(Actor::local_operator()).await?;
        store.recover_delivery_claims().await?;
        store.recover_launches().await?;
        if browser_desktop.is_some() {
            store.recover_desktop_claims().await?;
        }
        activate.send_replace(StartupState::Recovered);
        if let Some(managed) = &managed
            && !managed
                .wait(
                    &store_id,
                    install::StartupPermission::Dispatch,
                    wait.clone(),
                )
                .await?
        {
            return Ok(false);
        }
        Ok::<_, Failure>(!*wait.borrow())
    }
    .await;
    let mut workers = Vec::new();
    let hooks_host = host.clone();
    if matches!(&start, Ok(true)) {
        if let Some(host) = host.filter(|_| sends_allowed) {
            workers.push(tokio::spawn(dispatch_deliveries(
                store.clone(),
                host,
                store.subscribe_deliveries(),
                source.clone(),
            )));
        }
        workers.push(tokio::spawn(workflow::run(
            store.clone(),
            source,
            workflow,
            options.validation,
            wait.clone(),
        )));
        if !options.validation {
            workers.push(tokio::spawn(crate::checkout_setup::reconcile::run(
                store.clone(),
                profile.clone(),
                native,
                checkout_claude_profile,
            )));
        }
        if let Some(desktop) = browser_desktop {
            workers.push(desktop::start(store.clone(), browser, desktop, windows));
        }
        if let Some((host, paths)) = hooks_host.zip(installed) {
            workers.push(tokio::spawn(check_codex_hooks(host, paths)));
        }
        activate.send_replace(StartupState::Operational);
        println!("ReviewQ http://{browser}/");
    } else {
        shutdown.send_replace(true);
    }
    while listeners.join_next().await.is_some() {}
    stop.abort();
    for worker in workers {
        worker.abort();
        let _ = worker.await;
    }
    fs::remove_file(profile.join(RUNTIME))?;
    store.close(Duration::from_secs(10)).await?;
    if options.managed {
        install::bridge::clear(&endpoint)?;
    }
    start.map(|_| ())
}

pub(crate) fn claude_home() -> Result<PathBuf, Failure> {
    chosen_profile("CLAUDE_CONFIG_DIR", ".claude")
        .ok_or_else(|| "cannot resolve the Claude configuration directory".into())
}

fn chosen_profile(variable: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
        .filter(|path| path.is_absolute())
}

fn default_executable(name: &str, configured: Option<&str>, validation: bool) -> Option<PathBuf> {
    if validation {
        return None;
    }
    if let Some(configured) = configured {
        return runtime_settings::executable(configured);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())?;
    let executable = home.join(".local/bin").join(name);
    executable.is_file().then_some(executable)
}

/// Trusts ReviewQ's Codex hooks again when a Codex update or a moved entry left them
/// untrusted. A running install trusts them itself and says so, so this waits for it.
async fn check_codex_hooks(host: Arc<dyn NativeHost>, paths: install::paths::Paths) {
    paths.wait_unlocked().await;
    let _ = host.approve_hooks().await;
}

async fn dispatch_deliveries(
    store: Arc<dyn LeadStore>,
    host: Arc<dyn NativeHost>,
    mut commits: watch::Receiver<()>,
    sources: Arc<RuntimeSources>,
) {
    const CONCURRENCY: usize = 4;
    let mut running = tokio::task::JoinSet::new();
    let mut active = HashMap::new();
    loop {
        commits.borrow_and_update();
        let mut retry = false;
        if running.len() < CONCURRENCY {
            match store
                .recoverable_deliveries(HostProvider::Codex, None, CONCURRENCY as u16)
                .await
            {
                Ok(batch) => {
                    for candidate in batch {
                        if running.len() == CONCURRENCY {
                            break;
                        }
                        let id = candidate.cursor.delivery_id.clone();
                        if active.values().any(|running| running == &id) {
                            continue;
                        }
                        let store = store.clone();
                        let host = host.clone();
                        let sources = sources.clone();
                        let task = running.spawn(async move {
                            if !matches!(
                                tokio::time::timeout(
                                    Duration::from_secs(60),
                                    recover_delivery(store, host.as_ref(), candidate, sources)
                                )
                                .await,
                                Ok(Ok(()))
                            ) {
                                tokio::time::sleep(Duration::from_secs(1)).await;
                            }
                        });
                        active.insert(task.id(), id);
                    }
                }
                Err(_) => retry = true,
            }
        }
        let due = if running.len() < CONCURRENCY && !retry {
            match store.next_delivery_due(HostProvider::Codex).await {
                Ok(due) => due,
                Err(_) => {
                    retry = true;
                    None
                }
            }
        } else {
            None
        };
        let deadline = async {
            if retry {
                tokio::time::sleep(Duration::from_secs(1)).await;
            } else {
                wait_until_due(due).await;
            }
        };
        tokio::select! {
            result = commits.changed() => if result.is_err() { break; },
            result = running.join_next_with_id(), if !running.is_empty() => {
                if let Some(result) = result {
                    let id = result.map_or_else(|error| error.id(), |(id, ())| id);
                    active.remove(&id);
                }
            },
            () = deadline => {}
        }
    }
}

async fn wait_until_due(due: Option<i64>) {
    let Some(due) = due else {
        return std::future::pending::<()>().await;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .unwrap_or(due);
    let delay = due.saturating_sub(now).max(20) as u64;
    tokio::time::sleep(Duration::from_millis(delay)).await;
}

fn native_listener(port: u16) -> Result<TcpListener, std::io::Error> {
    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_only_v6(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port).into())?;
    socket.listen(128)?;
    TcpListener::from_std(socket.into())
}

async fn wait_for_stop(mut watch: watch::Receiver<bool>) {
    let _ = watch.wait_for(|value| *value).await;
}

fn create_profile(path: &Path) -> Result<(), Failure> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err("the profile must be a canonical absolute path".into());
    }
    let user = rustix::process::getuid().as_raw();
    let mut current = PathBuf::from("/");
    for name in path.components().filter_map(|part| match part {
        Component::Normal(name) => Some(name),
        _ => None,
    }) {
        current.push(name);
        match current.symlink_metadata() {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err("profile ancestors must be real directories".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if current
                    .parent()
                    .ok_or("missing profile parent")?
                    .metadata()?
                    .uid()
                    != user
                {
                    return Err("cannot create a profile below an unowned parent".into());
                }
                DirBuilder::new().mode(0o700).create(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    validate_profile(path)
}

fn validate_profile(path: &Path) -> Result<(), Failure> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.mode() & 0o777 != 0o700
        || path.canonicalize()? != path
    {
        return Err("expected an owner-only canonical profile directory".into());
    }
    Ok(())
}

/// The running service's endpoint, when the service runs this same build. A helper that runs
/// the service's own executable file proves that without reading it: while the service runs
/// the file, the kernel refuses to open it for writing (`ETXTBSY`), an upgrade replaces it by
/// rename, which gives a new inode, and any change to the file moves its change time. Any
/// other helper hashes itself as the service did at startup.
pub(crate) fn read_endpoint(profile: &Path) -> Result<ServiceEndpoint, Failure> {
    let endpoint = read_descriptor(profile)?;
    let identity = endpoint.identity();
    if executable_file().ok().as_ref() != Some(&identity.executable_file)
        && executable_digest()? != identity.build_digest
    {
        return Err(MISMATCH.into());
    }
    Ok(endpoint)
}

pub(crate) fn read_native_endpoint(profile: &Path) -> Result<ServiceEndpoint, Failure> {
    let endpoint = read_endpoint(profile)?;
    install::bridge::verify(profile, &endpoint)?;
    Ok(endpoint)
}

/// The running service's endpoint, when the service runs the build whose digest is `digest`.
pub(crate) fn read_endpoint_with_build(
    profile: &Path,
    digest: &str,
) -> Result<ServiceEndpoint, Failure> {
    let endpoint = read_descriptor(profile)?;
    if endpoint.identity().build_digest != digest {
        return Err(MISMATCH.into());
    }
    Ok(endpoint)
}

const MISMATCH: &str = "service endpoint does not match this build and profile";

/// The service descriptor in `profile`, when it is valid and names this profile.
fn read_descriptor(profile: &Path) -> Result<ServiceEndpoint, Failure> {
    validate_profile(profile)?;
    let file = private_file(&profile.join(RUNTIME))?;
    let mut data = Vec::new();
    file.take(MAX_DESCRIPTOR + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_DESCRIPTOR {
        return Err("service descriptor exceeds limit".into());
    }
    let endpoint: ServiceEndpoint = serde_json::from_slice(&data)?;
    if !endpoint.is_valid() || endpoint.identity().profile_id != profile_id(profile) {
        return Err(MISMATCH.into());
    }
    Ok(endpoint)
}

pub(crate) fn profile_id(path: &Path) -> String {
    hex(&Sha256::digest(path.as_os_str().as_bytes()))
}

/// The running executable's file: device, inode, size, and modification and change times.
fn executable_file() -> Result<String, Failure> {
    let file = fs::metadata("/proc/self/exe")?;
    Ok(format!(
        "{}:{}:{}:{}.{:09}:{}.{:09}",
        file.dev(),
        file.ino(),
        file.size(),
        file.mtime(),
        file.mtime_nsec(),
        file.ctime(),
        file.ctime_nsec()
    ))
}

fn executable_digest() -> Result<String, Failure> {
    let mut file = File::open("/proc/self/exe")?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 32 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex(&digest.finalize()));
        }
        digest.update(&buffer[..read]);
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").expect("writing to a String");
    }
    result
}

fn private_file(path: &Path) -> Result<File, Failure> {
    let flags = (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.nlink() != 1
        || meta.mode() & 0o777 != 0o600
    {
        return Err("expected an owner-only regular file".into());
    }
    Ok(file)
}

fn publish(profile: &Path, endpoint: &ServiceEndpoint) -> Result<(), Failure> {
    let descriptor = profile.join(RUNTIME);
    if descriptor.symlink_metadata().is_ok() {
        private_file(&descriptor)?;
    }
    let stage = profile.join(format!(".service.{}", endpoint.instance()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stage)?;
    let result = (|| -> Result<(), Failure> {
        serde_json::to_writer(&mut file, endpoint)?;
        writeln!(file)?;
        file.sync_all()?;
        fs::rename(&stage, descriptor)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(stage);
    }
    result
}
