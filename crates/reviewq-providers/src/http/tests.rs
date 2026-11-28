use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Url, header::HeaderMap};
use reviewq_app::access::{AppError, Service, SourceProblem};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::{Instant, timeout};

use super::SourceNetwork;

/// GitLab sends its rate-limit window on every answer. A 502 must not pause the origin until
/// that window resets, and a 200 that spends the window's last request must.
#[tokio::test]
async fn rate_window_pauses_only_when_spent() {
    let window = window(600);
    let (url, served) = serve(vec![
        format!("HTTP/1.1 502 Bad Gateway\r\n{window}ratelimit-remaining: 1999\r\n"),
        format!("HTTP/1.1 200 OK\r\n{window}ratelimit-remaining: 0\r\n"),
    ])
    .await;
    let network = SourceNetwork::new().unwrap();

    let (_, body) = network
        .bounded_response(Service::GitLab, url.clone(), HeaderMap::new(), 64, None)
        .await
        .unwrap();
    assert_eq!(body, b"{}");
    assert_eq!(served.load(Ordering::SeqCst), 2);
    assert!(network.paused_for(&url) > Duration::from_secs(500));

    let waited = network
        .bounded_response(Service::GitLab, url, HeaderMap::new(), 64, None)
        .await;
    assert_eq!(
        waited.err(),
        Some(AppError::Source(SourceProblem::Wait(Service::GitLab)))
    );
    assert_eq!(served.load(Ordering::SeqCst), 2);
}

/// Calls that wait out one origin's pause hold none of the four shared permits, so another
/// service's call goes out at once.
#[tokio::test]
async fn paused_origin_holds_no_permit() {
    let (gitlab, _) = serve(vec![format!(
        "HTTP/1.1 200 OK\r\n{}ratelimit-remaining: 0\r\n",
        window(10)
    )])
    .await;
    let (jira, served) = serve(vec!["HTTP/1.1 200 OK\r\n".into()]).await;
    let network = Arc::new(SourceNetwork::new().unwrap());
    network
        .bounded_response(Service::GitLab, gitlab.clone(), HeaderMap::new(), 64, None)
        .await
        .unwrap();
    let waiting: Vec<_> = (0..6)
        .map(|_| {
            let (network, url) = (network.clone(), gitlab.clone());
            tokio::spawn(async move {
                network
                    .bounded_response(Service::GitLab, url, HeaderMap::new(), 64, None)
                    .await
            })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = Instant::now();
    timeout(
        Duration::from_secs(2),
        network.bounded_response(Service::Jira, jira, HeaderMap::new(), 64, None),
    )
    .await
    .expect("Jira waited behind GitLab's pause")
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(served.load(Ordering::SeqCst), 1);
    assert!(waiting.iter().all(|call| !call.is_finished()));
    waiting.iter().for_each(|call| call.abort());
}

/// GitLab's rate-limit headers with a reset `seconds` from now.
fn window(seconds: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let reset = now.as_secs() + seconds;
    format!("ratelimit-limit: 2000\r\nratelimit-reset: {reset}\r\n")
}

/// Serves each status line and headers in `answers` once, in order, with a `{}` JSON body,
/// on a loopback origin of its own. Returns a URL there and the count of requests served.
async fn serve(answers: Vec<String>) -> (Url, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = Url::parse(&format!("http://{address}/api/v4/user")).unwrap();
    let served = Arc::new(AtomicUsize::new(0));
    let count = served.clone();
    tokio::spawn(async move {
        for answer in answers {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0; 1];
                socket.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            count.fetch_add(1, Ordering::SeqCst);
            let reply = format!(
                "{answer}content-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}"
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (url, served)
}
