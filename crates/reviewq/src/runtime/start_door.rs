//! The installed service's side of the start door. systemd holds the start port while
//! ReviewQ is stopped and hands its socket to the service at every start while it listens.
//! The service checks that a connection that started it came from the stopped page, answers
//! the door while it runs, and repairs the socket unit after each installed start.

use std::net::TcpListener as StdListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::Response;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::install::{self, paths::Paths};
use crate::runtime_settings;

/// How long the first-connection check reads the connections waiting at startup, in all.
const FIRST_CONNECTIONS: Duration = Duration::from_secs(2);

/// Takes the one socket systemd passed this process, marked close-on-exec, and removes
/// `LISTEN_PID` and `LISTEN_FDS` from the environment, so no child inherits the socket or the
/// claim to it. Call it before any thread starts: it edits the environment.
pub(crate) fn take() -> Option<StdListener> {
    let mut passed = listenfd::ListenFd::from_env();
    if passed.len() != 1 {
        return None;
    }
    match passed.take_tcp_listener(0) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("Ignored the start socket systemd passed: {error}");
            None
        }
    }
}

/// The start socket this service was handed, with the router that answers it.
pub(super) struct Door {
    listener: StdListener,
    router: Router,
}

impl Door {
    /// The door for the dashboard on `browser_port`. A socket on another port is reported;
    /// the repair after this start moves it.
    pub(super) fn new(listener: StdListener, browser_port: u16) -> Option<Self> {
        let expected = runtime_settings::start_port(browser_port)?;
        match listener.local_addr() {
            Ok(address) if address.port() != expected => println!(
                "The start port is {}, expected {expected}; repairing it.",
                address.port()
            ),
            Ok(_) => {}
            Err(error) => {
                eprintln!("Ignored an unreadable start socket: {error}");
                return None;
            }
        }
        listener.set_nonblocking(true).ok()?;
        Some(Self {
            listener,
            router: reviewq_transport::start_door(browser_port, expected),
        })
    }

    /// The first-connection check, made before the store opens. Answers each connection
    /// already waiting, for up to two seconds in all. Returns `false` when connections were
    /// waiting and none was a valid start: the caller then exits cleanly, so a stray local
    /// connection or another site's page cannot undo a Quit. With nothing waiting, ReviewQ
    /// was started by a login, `reviewq start` or an install, and the start goes on.
    pub(super) async fn admits_start(&self) -> bool {
        let started = Arc::new(AtomicBool::new(false));
        let mut waiting = tokio::task::JoinSet::new();
        // A connection that closed before it could be read still woke the service.
        let mut seen = false;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    seen = true;
                    if let Ok(stream) = stream
                        .set_nonblocking(true)
                        .and_then(|()| TcpStream::from_std(stream))
                    {
                        waiting.spawn(answer(stream, self.router.clone(), started.clone()));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionAborted => seen = true,
                Err(_) => break,
            }
        }
        if !seen {
            return true;
        }
        let deadline = tokio::time::sleep(FIRST_CONNECTIONS);
        tokio::pin!(deadline);
        while !started.load(Ordering::Acquire) {
            tokio::select! {
                next = waiting.join_next() => if next.is_none() { break },
                () = &mut deadline => break,
            }
        }
        waiting.abort_all();
        if started.load(Ordering::Acquire) {
            return true;
        }
        println!("Ignored a start request that did not come from ReviewQ's page.");
        false
    }

    /// Answers the door until shutdown, as the dashboard's listeners do.
    pub(super) async fn serve(self, shutdown: watch::Receiver<bool>) {
        if let Ok(listener) = TcpListener::from_std(self.listener) {
            super::http::serve(listener, self.router, shutdown).await;
        }
    }
}

/// Answers one waiting connection, recording a valid start in `started`.
async fn answer(stream: TcpStream, router: Router, started: Arc<AtomicBool>) {
    let router = router.layer(axum::middleware::map_response(move |response: Response| {
        let started = started.clone();
        async move {
            if response.status() == StatusCode::NO_CONTENT {
                started.store(true, Ordering::Release);
            }
            response
        }
    }));
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(FIRST_CONNECTIONS)
        .keep_alive(false);
    let _ = builder
        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(router))
        .await;
}

/// Repairs the start socket after an installed start. Skipped while an install holds the
/// installation lock, since the install handles the socket itself.
pub(super) async fn repair(paths: Paths) {
    match paths.try_lock() {
        Ok(Some(lock)) => {
            if let Err(error) = install::repair_start_socket(&paths).await {
                eprintln!("ReviewQ could not repair its start door: {error}");
            }
            drop(lock);
        }
        Ok(None) => {}
        Err(error) => eprintln!("ReviewQ could not repair its start door: {error}"),
    }
}
