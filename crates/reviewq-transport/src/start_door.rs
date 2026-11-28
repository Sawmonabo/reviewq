//! The start door's answer. systemd holds the start port and runs one small door process for
//! each connection, which answers with this router and starts the service on a valid start.
//! Only the stopped page's Start button counts as a start: a `POST /start` addressed to the
//! start port from the dashboard's own origin. It sends no CORS headers, since the page sends
//! the request with `mode: "no-cors"` and never reads the answer.

use axum::Router;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};

/// The router for the start door: `204` for a valid start, `404` for anything else. A start
/// is valid when it is `POST /start` with exactly one Host, `127.0.0.1:<start_port>`, and
/// exactly one Origin, `http://127.0.0.1:<browser_port>`.
pub fn start_door(browser_port: u16, start_port: u16) -> Router {
    let host = format!("127.0.0.1:{start_port}");
    let origin = format!("http://127.0.0.1:{browser_port}");
    Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap| {
        let valid = method == Method::POST
            && uri.path() == "/start"
            && uri.query().is_none()
            && only(&headers, header::HOST, &host)
            && only(&headers, header::ORIGIN, &origin);
        async move {
            if valid {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        }
    })
}

fn only(headers: &HeaderMap, name: header::HeaderName, expected: &str) -> bool {
    let mut values = headers.get_all(name).iter();
    values.next().is_some_and(|value| value == expected) && values.next().is_none()
}
