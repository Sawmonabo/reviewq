use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use axum::Router;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

const IDLE: Duration = Duration::from_secs(30);
const DRAIN: Duration = Duration::from_secs(3);
/// Accept failures less than this apart are one episode, logged once.
const EPISODE: Duration = Duration::from_secs(60);
/// The most open files the service asks for: far above a burst of browser and MCP
/// connections, and no more than the hard limits systems commonly set.
const OPEN_FILES: u64 = 65_536;
/// How many new connections each listener holds until the service accepts them, so a burst
/// of agent calls waits instead of being dropped. Linux caps it at `net.core.somaxconn`,
/// 4096 by default; Tokio's own bind asks for 128.
const ACCEPT_BACKLOG: i32 = 4096;

/// IPv6 loopback takes the same port, so `localhost` reaches the service on either family.
/// Without IPv6 loopback the service runs on IPv4 alone; a port in use there still fails,
/// unless the system chose it for IPv4 alone: then it chooses again, a few times.
/// `choose` is the step a port-in-use message names.
pub(super) fn listeners(
    port: u16,
    choose: &str,
) -> std::io::Result<(TcpListener, Option<TcpListener>)> {
    let mut choices = 0;
    loop {
        let v4 = loopback(Ipv4Addr::LOCALHOST.into(), port, choose)?;
        let chosen = v4.local_addr()?.port();
        match loopback(Ipv6Addr::LOCALHOST.into(), chosen, choose) {
            Ok(listener) => return Ok((v4, Some(listener))),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::AddrNotAvailable | ErrorKind::Unsupported
                ) || error.raw_os_error()
                    == Some(rustix::io::Errno::AFNOSUPPORT.raw_os_error()) =>
            {
                return Ok((v4, None));
            }
            Err(error) if port == 0 && error.kind() == ErrorKind::AddrInUse && choices < 8 => {
                choices += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Listens on loopback `ip` at `port`, with `ACCEPT_BACKLOG`. An IPv6 listener takes IPv6
/// only, so it never answers for the IPv4 port. Address reuse lets a restart bind while
/// connections closed by the previous service linger, as Tokio's own bind allows. When
/// another program holds the address, the error keeps the kind `AddrInUse` and names the
/// address and the step: stop that program, or `choose` another port.
pub(super) fn loopback(ip: IpAddr, port: u16, choose: &str) -> std::io::Result<TcpListener> {
    let address = SocketAddr::new(ip, port);
    listen(address).map_err(|error| match error.kind() {
        ErrorKind::AddrInUse => std::io::Error::new(
            ErrorKind::AddrInUse,
            format!(
                "{address} is already in use, by a ReviewQ already running on this port or by another program. Stop that one, or {choose}."
            ),
        ),
        _ => error,
    })
}

fn listen(address: SocketAddr) -> std::io::Result<TcpListener> {
    let ip = address.ip();
    let socket = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    if ip.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(ACCEPT_BACKLOG)?;
    TcpListener::from_std(socket.into())
}

/// Raises the soft open-file limit to the hard one, at most `OPEN_FILES`. A service starts
/// with a soft limit of 1024, which a burst of connections reaches. Child processes inherit
/// the raised limit. Says nothing unless raising it fails.
pub(super) fn raise_open_files() {
    let limit = getrlimit(Resource::Nofile);
    let wanted = limit
        .maximum
        .map_or(OPEN_FILES, |hard| hard.min(OPEN_FILES));
    let Some(soft) = limit.current.filter(|soft| *soft < wanted) else {
        return;
    };
    let raised = Rlimit {
        current: Some(wanted),
        maximum: limit.maximum,
    };
    if let Err(error) = setrlimit(Resource::Nofile, raised) {
        eprintln!("ReviewQ could not raise its open-file limit from {soft} to {wanted}: {error}");
    }
}

pub(super) async fn serve(listener: TcpListener, router: Router, shutdown: watch::Receiver<bool>) {
    let mut tasks = JoinSet::new();
    let stop = super::wait_for_stop(shutdown.clone());
    tokio::pin!(stop);
    // Kept across iterations: each one starts a new `accept`.
    let mut failed = None;
    loop {
        tokio::select! {
            () = &mut stop => break,
            _ = tasks.join_next(), if !tasks.is_empty() => (),
            socket = accept(&listener, &mut failed) => {
                tasks.spawn(serve_connection(socket, router.clone(), shutdown.clone()));
            }
        }
    }
    if tokio::time::timeout(DRAIN, async { while tasks.join_next().await.is_some() {} })
        .await
        .is_err()
    {
        tasks.shutdown().await;
    }
}

/// The next connection. A failed accept, such as one past the open-file limit, is retried
/// every 250 ms. It is logged once per episode: `failed` keeps when the last one failed, and a
/// failure within `EPISODE` of it belongs to the same episode.
async fn accept(listener: &TcpListener, failed: &mut Option<Instant>) -> TcpStream {
    loop {
        match listener.accept().await {
            Ok((socket, _)) => return socket,
            Err(error) => {
                if failed.is_none_or(|last| last.elapsed() >= EPISODE) {
                    eprintln!("ReviewQ could not accept a connection and keeps retrying: {error}");
                }
                *failed = Some(Instant::now());
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

async fn serve_connection(socket: TcpStream, router: Router, shutdown: watch::Receiver<bool>) {
    let mut builder = http1::Builder::new();
    builder.timer(TokioTimer::new()).header_read_timeout(IDLE);
    let connection = builder
        .serve_connection(TokioIo::new(socket), TowerToHyperService::new(router))
        .with_upgrades();
    tokio::pin!(connection);
    tokio::select! {
        _ = &mut connection => (),
        () = super::wait_for_stop(shutdown) => {
            connection.as_mut().graceful_shutdown();
            let _ = connection.await;
        }
    }
}
