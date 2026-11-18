use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reviewq_app::access::Actor;
use reviewq_app::sources::ReviewSourceReader;
use reviewq_app::workspace::{WorkspaceConfig, WorkspaceStore};
use reviewq_domain::session::AbsolutePath;
use reviewq_providers::GitLab;
use reviewq_store::SqliteStore;
use reviewq_transport::ServiceEndpoint;
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
    create_profile(&profile)?;
    let web = options.web.canonicalize()?;
    if !web.join("index.html").is_file() {
        return Err("build the browser assets before serving".into());
    }
    if options.validation && profile == self::profile(None)? {
        return Err("validation requires a separate private runtime".into());
    }
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
    let endpoint = ServiceEndpoint::new(browser, native)?;
    let store = Arc::new(
        SqliteStore::open(AbsolutePath::from_canonical(
            profile.join("reviewq.sqlite3"),
        )?)
        .await?,
    );

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
            Some(GitLab::new(origin, None)?)
        }
        Some(origin) => {
            let token = std::env::var("REVIEWQ_GITLAB_TOKEN").ok();
            Some(GitLab::new(origin, token.as_deref())?)
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
    let router = reviewq_transport::router(store.clone(), source, endpoint.clone(), &web);
    publish(&profile, &endpoint)?;
    println!("ReviewQ http://{browser}/");

    let (shutdown, wait) = watch::channel(false);
    let listener_shutdown = wait.clone();
    let stop = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
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
    fs::remove_file(profile.join(RUNTIME))?;
    store.close(Duration::from_secs(10)).await?;
    result?;
    Ok(())
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
    if !path.is_absolute() {
        return Err("the profile must be an absolute path".into());
    }
    match fs::symlink_metadata(path) {
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path)?;
        }
        Err(error) => return Err(error.into()),
    }
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
    let file = private_file(&profile.join(RUNTIME))?;
    let mut data = Vec::new();
    file.take(MAX_DESCRIPTOR + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_DESCRIPTOR {
        return Err("service descriptor exceeds limit".into());
    }
    let endpoint: ServiceEndpoint = serde_json::from_slice(&data)?;
    if !endpoint.is_valid() {
        return Err("invalid service endpoint".into());
    }
    Ok(endpoint)
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
