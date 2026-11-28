//! `reviewq start-door`: one connection to the start door. systemd accepts each connection on
//! `reviewq-start.socket` and runs this with the connection as standard input. It answers one
//! HTTP/1 request with the start door's router and, for a valid start, asks systemd to start
//! `reviewq.service`. It reads no settings, store or install state, and writes nothing to
//! standard output; its reply goes out over the connection.

use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::Response;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;

use crate::{runtime_settings, service};

type Failure = Box<dyn std::error::Error + Send + Sync>;

/// How long the request's headers may take to arrive.
const HEADERS: Duration = Duration::from_secs(2);
/// How long the whole connection may take.
const CONNECTION: Duration = Duration::from_secs(5);

/// Answers the connection on standard input. Fails only when standard input is not a TCP
/// connection; a stray or invalid request, or a failed start, still exits with status 0.
pub(crate) async fn run() -> Result<(), Failure> {
    let stream = std::net::TcpStream::from(std::io::stdin().as_fd().try_clone_to_owned()?);
    let door_port = stream
        .local_addr()
        .map_err(|_| "The start door's standard input is not a TCP connection.")?
        .port();
    let Some(browser_port) = runtime_settings::browser_port(door_port) else {
        return Ok(());
    };
    stream.set_nonblocking(true)?;
    let stream = tokio::net::TcpStream::from_std(stream)?;
    let started = Arc::new(AtomicBool::new(false));
    let recorded = started.clone();
    let router = reviewq_transport::start_door(browser_port, door_port).layer(
        axum::middleware::map_response(move |response: Response| {
            let recorded = recorded.clone();
            async move {
                if response.status() == StatusCode::NO_CONTENT {
                    recorded.store(true, Ordering::Release);
                }
                response
            }
        }),
    );
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HEADERS)
        .keep_alive(false);
    let connection =
        builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(router));
    let _ = tokio::time::timeout(CONNECTION, connection).await;
    if started.load(Ordering::Acquire)
        && let Err(error) = service::start_without_waiting().await
    {
        eprintln!("The start door could not start ReviewQ: {error}");
    }
    Ok(())
}
