use std::time::{Duration, Instant};

use axum::Router;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
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
