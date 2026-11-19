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
use reviewq_app::sources::{ReviewSourceReader, SourceRefreshStore};
use reviewq_app::workspace::{WorkspaceConfig, WorkspaceStore};
use reviewq_domain::session::{AbsolutePath, HostProvider};
use reviewq_providers::{Claude, Codex, FileHistory, GitLab};
use reviewq_store::SqliteStore;
use reviewq_transport::{ServiceEndpoint, ServiceIdentity};
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::cli::Serve;

const RUNTIME: &str = "service.json";
const MAX_DESCRIPTOR: u64 = 8192;
type Failure = Box<dyn std::error::Error + Send + Sync>;

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
    let web = match options.web.as_ref() {
        Some(path) => path.clone(),
        None if options.validation => {
            return Err("validation requires an explicit web asset directory".into());
        }
        None => profile.join("current/web"),
    }
    .canonicalize()?;
    if !web.join("index.html").is_file() {
        return Err("build the browser assets before serving".into());
    }
    if options.validation && profile == self::profile(None)? {
        return Err("validation requires a separate private runtime".into());
    }
    create_profile(&profile)?;
    let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, options.port)).await?;
    let v6 = match native_listener(options.native_port) {
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
    let build_digest = executable_digest()?;
    let store = Arc::new(
        SqliteStore::open(AbsolutePath::from_canonical(
            profile.join("reviewq.sqlite3"),
        )?)
        .await?,
    );
    let identity = store.identity();
    let endpoint = ServiceEndpoint::new(
        browser,
        native,
        ServiceIdentity {
            build_digest,
            profile_id: profile_id(&profile),
            store_id: identity.store_id.to_string(),
            store_application_id: identity.application_id,
            store_schema: identity.format,
        },
    )?;
    store.recover_refresh(Actor::local_operator()).await?;
    store.recover_delivery_claims().await?;

    let source = match options.gitlab.as_ref() {
        Some(origin) if options.validation => {
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
            Some(GitLab::new(origin, None, profile.clone())?)
        }
        Some(origin) => {
            let token = std::env::var("REVIEWQ_GITLAB_TOKEN").ok();
            Some(GitLab::new(origin, token.as_deref(), profile.clone())?)
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
    let source = source.map(|source| Arc::new(source) as Arc<dyn ReviewSourceReader>);
    let host = match (options.codex_bin, options.codex_home) {
        (Some(executable), Some(home)) => {
            let metadata = home.canonicalize()?.metadata()?;
            if metadata.uid() != rustix::process::getuid().as_raw() {
                return Err("the selected Codex profile must belong to the service user".into());
            }
            let settings = options.codex_model.zip(options.codex_effort);
            Some(Arc::new(Codex::new(executable, home, settings)?) as Arc<dyn NativeHost>)
        }
        _ => None,
    };
    let sends_allowed = !options.validation && host.is_some();
    let claude = match options.claude_home {
        Some(home) => Some(home),
        None if !options.validation => claude_home().ok().filter(|path| path.is_dir()),
        None => None,
    }
    .map(Claude::new)
    .transpose()?
    .map(|claude| Arc::new(claude) as Arc<dyn ClaudeCompanion>);
    let delivery = host.as_ref().filter(|_| sends_allowed).map(|host| {
        tokio::spawn(dispatch_deliveries(
            store.clone(),
            host.clone(),
            store.subscribe_deliveries(),
        ))
    });
    let (shutdown, wait) = watch::channel(false);
    let router = reviewq_transport::router(
        store.clone(),
        source,
        host,
        Arc::new(FileHistory::default()),
        reviewq_transport::ServiceRuntime {
            sends_allowed,
            claude,
            workflow: None,
            endpoint: endpoint.clone(),
            web: &web,
            commits: store.subscribe(),
            shutdown: wait.clone(),
        },
    );
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    publish(&profile, &endpoint)?;
    println!("ReviewQ http://{browser}/");

    let listener_shutdown = wait.clone();
    let stop = tokio::spawn(async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        shutdown.send_replace(true);
    });
    let serve_v4 = axum::serve(v4, router.clone()).with_graceful_shutdown(wait_for_stop(wait));
    let result = match v6 {
        Some(v6) => {
            let serve_v6 =
                axum::serve(v6, router).with_graceful_shutdown(wait_for_stop(listener_shutdown));
            tokio::try_join!(serve_v4, serve_v6).map(|_| ())
        }
        None => serve_v4.await,
    };
    stop.abort();
    if let Some(delivery) = delivery {
        delivery.abort();
        let _ = delivery.await;
    }
    fs::remove_file(profile.join(RUNTIME))?;
    store.close(Duration::from_secs(10)).await?;
    result?;
    Ok(())
}

pub(crate) fn claude_home() -> Result<PathBuf, Failure> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "cannot resolve the Claude configuration directory".into())
}

async fn dispatch_deliveries(
    store: Arc<dyn LeadStore>,
    host: Arc<dyn NativeHost>,
    mut commits: watch::Receiver<()>,
) {
    let mut after = None;
    loop {
        commits.borrow_and_update();
        let batch = match store
            .recoverable_deliveries(HostProvider::Codex, after.take(), 16)
            .await
        {
            Ok(batch) => batch,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let mut failed = false;
        let mut running = tokio::task::JoinSet::new();
        for candidate in batch {
            after = Some(candidate.cursor.clone());
            if running.len() == 4 {
                failed |= !matches!(running.join_next().await, Some(Ok(true)));
            }
            let store = store.clone();
            let host = host.clone();
            running.spawn(async move {
                matches!(
                    tokio::time::timeout(
                        Duration::from_secs(60),
                        recover_delivery(store, host.as_ref(), candidate)
                    )
                    .await,
                    Ok(Ok(()))
                )
            });
        }
        while let Some(result) = running.join_next().await {
            failed |= !matches!(result, Ok(true));
        }
        if failed {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if after.is_some() {
            continue;
        }
        let due = match store.next_delivery_due(HostProvider::Codex).await {
            Ok(due) => due,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let wait_due = async {
            match due {
                Some(due) => {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .and_then(|value| i64::try_from(value.as_millis()).ok())
                        .unwrap_or(due);
                    let delay = due.saturating_sub(now).max(20) as u64;
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            result = commits.changed() => if result.is_err() { break; },
            () = wait_due => {}
        }
    }
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

pub(crate) fn read_endpoint(profile: &Path) -> Result<ServiceEndpoint, Failure> {
    validate_profile(profile)?;
    let file = private_file(&profile.join(RUNTIME))?;
    let mut data = Vec::new();
    file.take(MAX_DESCRIPTOR + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_DESCRIPTOR {
        return Err("service descriptor exceeds limit".into());
    }
    let endpoint: ServiceEndpoint = serde_json::from_slice(&data)?;
    let identity = endpoint.identity();
    if !endpoint.is_valid()
        || identity.profile_id != profile_id(profile)
        || identity.build_digest != executable_digest()?
    {
        return Err("service endpoint does not match this build and profile".into());
    }
    Ok(endpoint)
}

fn profile_id(path: &Path) -> String {
    hex(&Sha256::digest(path.as_os_str().as_bytes()))
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
        return Err("expected an owner-only service descriptor".into());
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
