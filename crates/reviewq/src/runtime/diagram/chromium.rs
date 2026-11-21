//! One authorized drawing through the installed Chromium and its matching web assets.

use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::reports::{DrawingOutput, DrawingRenderer, MAX_DRAWING_BYTES};
use reviewq_domain::identity::ReportHash;
use rustix::process::{Pid, Signal, kill_process_group};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tower_http::services::{ServeDir, ServeFile};

const PAGE: &str = "/diagram-producer.html";
const REQUEST_BYTES: usize = 256 * 1024;
const DEADLINE: Duration = Duration::from_secs(12);
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'";

pub(in crate::runtime) struct Chromium {
    executable: PathBuf,
    web: PathBuf,
    jobs: PathBuf,
    disk: Arc<super::disk::Disk>,
}

impl Chromium {
    pub(in crate::runtime) fn open(
        executable: PathBuf,
        web: &Path,
        profile: &Path,
    ) -> Option<Self> {
        if !web.join(&PAGE[1..]).is_file() || !web.join("assets").is_dir() {
            return None;
        }
        let jobs = super::disk::private_directory(profile, "diagram-jobs")?;
        let disk = Arc::new(super::disk::Disk::open(profile)?);
        let owner = rustix::process::getuid().as_raw();
        let info = fs::symlink_metadata(&jobs).ok()?;
        if !info.is_dir() || info.uid() != owner || info.mode() & 0o077 != 0 {
            return None;
        }
        for item in fs::read_dir(&jobs).ok()? {
            let item = item.ok()?;
            let name = item.file_name();
            if let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix("render-"))
                && suffix.len() == 12
                && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                && item.file_type().ok()?.is_dir()
                && fs::symlink_metadata(item.path()).ok()?.uid() == owner
            {
                fs::remove_dir_all(item.path()).ok()?;
            }
        }
        Some(Self {
            executable,
            web: web.to_owned(),
            jobs,
            disk,
        })
    }

    async fn produce(&self, source: String) -> Result<DrawingOutput, AppError> {
        let profile = tempfile::Builder::new()
            .prefix("render-")
            .rand_bytes(12)
            .tempdir_in(&self.jobs)
            .map_err(|_| AppError::Unavailable)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| AppError::Unavailable)?;
        let address = listener.local_addr().map_err(|_| AppError::Unavailable)?;
        let input = token()?;
        let output = token()?;
        let (send, receive) = oneshot::channel();
        let job = Arc::new(Job {
            address,
            input: input.clone(),
            output: output.clone(),
            source: Mutex::new(Some(Input { source, output })),
            send: Mutex::new(Some(send)),
        });
        let router = Router::new()
            .route_service(PAGE, ServeFile::new(self.web.join(&PAGE[1..])))
            .nest_service("/assets", ServeDir::new(self.web.join("assets")))
            .route("/job/input", get(input_page))
            .route(
                "/job/output",
                post(output_page).layer(DefaultBodyLimit::max(REQUEST_BYTES)),
            )
            .with_state(job.clone())
            .layer(middleware::from_fn_with_state(job, local_only));
        let server = Server(tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        }));
        let mut child = Command::new(&self.executable)
            .args([
                "--headless",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-extensions",
                "--disable-sync",
                "--disable-quic",
                "--disk-cache-size=1",
                "--media-cache-size=1",
                "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
                "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1",
                "--disable-gpu",
                "--disable-crash-reporter",
                "--disable-breakpad",
                "--no-default-browser-check",
                "--no-first-run",
            ])
            .arg(format!("--user-data-dir={}", profile.path().display()))
            // The job listener also rejects proxy requests. Only its exact origin is direct.
            .arg(format!("--proxy-server=http://{address}"))
            .arg(format!("--proxy-bypass-list=<-loopback>;http://{address}"))
            .arg(format!("http://{address}{PAGE}#{input}"))
            .current_dir(profile.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", profile.path())
            .env("LANG", "C.UTF-8")
            .env("XDG_CONFIG_HOME", profile.path())
            .env("XDG_CACHE_HOME", profile.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| AppError::Unavailable)?;
        let group = ProcessGroup(
            child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .and_then(Pid::from_raw),
        );
        let outcome = tokio::time::timeout(DEADLINE, async {
            tokio::select! {
                result = receive => result.unwrap_or(Err(AppError::Unavailable)),
                _ = child.wait() => Err(AppError::Unavailable),
            }
        })
        .await
        .unwrap_or(Err(AppError::Unavailable));
        drop(server);
        if let Some(group) = group.0 {
            let _ = kill_process_group(group, Signal::TERM);
        }
        let _ = tokio::time::timeout(Duration::from_millis(750), child.wait()).await;
        drop(group);
        let _ = child.wait().await;
        outcome
    }
}

impl DrawingRenderer for Chromium {
    fn draw(&self, source: String) -> AppFuture<'_, DrawingOutput> {
        Box::pin(self.produce(source))
    }

    fn ready_font(&self, source: &ReportHash) -> Result<Option<ReportHash>, AppError> {
        self.disk.font(source)
    }

    fn publish(&self, source: ReportHash, drawing: DrawingOutput) -> AppFuture<'_, ()> {
        let disk = self.disk.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || disk.publish(source, drawing))
                .await
                .map_err(|_| AppError::Unavailable)?
        })
    }

    fn read(&self, source: ReportHash, font: ReportHash) -> AppFuture<'_, String> {
        let disk = self.disk.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || disk.read(&source, &font))
                .await
                .map_err(|_| AppError::Unavailable)?
        })
    }

    fn expire(&self) -> AppFuture<'_, ()> {
        let disk = self.disk.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || disk.expire())
                .await
                .map_err(|_| AppError::Unavailable)?
        })
    }
}

struct Server(JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct ProcessGroup(Option<Pid>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _ = kill_process_group(group, Signal::KILL);
        }
    }
}

struct Job {
    address: SocketAddr,
    input: String,
    output: String,
    source: Mutex<Option<Input>>,
    send: Mutex<Option<oneshot::Sender<Result<DrawingOutput, AppError>>>>,
}

#[derive(Serialize)]
struct Input {
    source: String,
    output: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Output {
    Ready { svg: String, font: String },
    Declined,
}

impl Output {
    fn verify(self) -> Result<DrawingOutput, AppError> {
        let Self::Ready { svg, font } = self else {
            return Err(AppError::InvalidRequest);
        };
        if svg.len() > MAX_DRAWING_BYTES {
            return Err(AppError::InvalidRequest);
        }
        let font = font.parse().map_err(|_| AppError::InvalidRequest)?;
        Ok(DrawingOutput { svg, font })
    }
}

async fn local_only(State(job): State<Arc<Job>>, request: Request, next: Next) -> Response {
    if request.uri().scheme().is_some()
        || request.uri().authority().is_some()
        || request
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            != Some(job.address.to_string().as_str())
        || !matches!(*request.method(), Method::GET | Method::POST)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn input_page(
    State(job): State<Arc<Job>>,
    headers: HeaderMap,
) -> Result<Json<Input>, StatusCode> {
    if !authorized(&headers, &job.input) {
        return Err(StatusCode::FORBIDDEN);
    }
    job.source
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .take()
        .map(Json)
        .ok_or(StatusCode::GONE)
}

async fn output_page(
    State(job): State<Arc<Job>>,
    headers: HeaderMap,
    Json(output): Json<Output>,
) -> Result<StatusCode, StatusCode> {
    if !authorized(&headers, &job.output)
        || job
            .source
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
            .is_some()
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let send = job
        .send
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .take()
        .ok_or(StatusCode::GONE)?;
    send.send(output.verify()).map_err(|_| StatusCode::GONE)?;
    Ok(StatusCode::NO_CONTENT)
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        == Some(token)
}

fn token() -> Result<String, AppError> {
    let mut value = [0; 32];
    getrandom::fill(&mut value).map_err(|_| AppError::Unavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}
