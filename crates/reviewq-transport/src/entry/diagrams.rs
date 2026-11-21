//! Disposable official drawings. A saved digest is never a document permission.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::Response;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::reports::{DRAWING_RECIPE, DrawingRenderer, ReportReference, mermaid_sources};
use reviewq_domain::identity::{ReportHash, RunId};
use reviewq_protocol::PageCursorDto;
use reviewq_protocol::reports::{DocumentDisplay, DrawingHint, ReportHashDto};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use super::error::ApiPath as Path;
use super::history::{ContentQuery, drawing_source, read_content};
use super::pages::parse;
use super::{ApiError, Entry};

const TOKEN: &[u8] = b"reviewq-official-drawing-v1";
const BACKOFF: Duration = Duration::from_secs(15);
const BACKOFF_RECORDS: usize = 32;
const PENDING: usize = 4;
const COLLECT: Duration = Duration::from_secs(60);

pub(super) struct Cache {
    renderer: Option<Arc<dyn DrawingRenderer>>,
    state: Mutex<Producer>,
}

#[derive(Default)]
struct Producer {
    running: bool,
    active: Option<ReportHash>,
    pending: VecDeque<Job>,
    retry: VecDeque<(ReportHash, Instant)>,
}

struct Job {
    actor: Actor,
    owner: Owner,
    source: String,
    hash: ReportHash,
    font: Option<ReportHash>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub(super) enum Owner {
    Report {
        run: String,
        receipt: String,
        report_hash: String,
    },
    Native {
        run: String,
        session: String,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<NativeWindowOwner>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub(super) struct NativeWindowOwner {
    pub(super) byte_start: u64,
    pub(super) sha256: String,
}

#[derive(Deserialize, Serialize)]
struct Locator {
    owner: Owner,
    source_hash: ReportHashDto,
    recipe: String,
    font: String,
}

impl Owner {
    pub(super) fn report(run: &RunId, reference: &ReportReference) -> Self {
        Self::Report {
            run: run.to_string(),
            receipt: reference.receipt_id.to_string(),
            report_hash: reference.hash.to_string(),
        }
    }
}

impl Cache {
    pub(super) fn enabled(&self) -> bool {
        self.renderer.is_some()
    }

    pub(super) fn new(
        renderer: Option<Arc<dyn DrawingRenderer>>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Self {
        if let Some(renderer) = &renderer {
            let weak = Arc::downgrade(renderer);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        () = tokio::time::sleep(COLLECT) => {}
                    }
                    let Some(renderer) = weak.upgrade() else {
                        break;
                    };
                    let _ = renderer.expire().await;
                }
            });
        }
        Self {
            renderer,
            state: Mutex::new(Producer::default()),
        }
    }
}

pub(super) fn client_font(headers: &HeaderMap) -> Option<ReportHash> {
    headers
        .get("x-reviewq-drawing-font")?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

pub(super) fn observe(
    entry: &Arc<Entry>,
    actor: &Actor,
    owner: Owner,
    markdown: &str,
    font: Option<&ReportHash>,
) -> Option<DocumentDisplay> {
    let prepare = entry.drawings.renderer.as_ref()?.can_draw(font);
    observe_markdown(entry, actor, owner, markdown, prepare, font)
}

pub(super) fn known(
    entry: &Arc<Entry>,
    actor: &Actor,
    owner: Owner,
    markdown: &str,
) -> Option<DocumentDisplay> {
    observe_markdown(entry, actor, owner, markdown, false, None)
}

fn observe_markdown(
    entry: &Arc<Entry>,
    actor: &Actor,
    owner: Owner,
    markdown: &str,
    prepare: bool,
    font: Option<&ReportHash>,
) -> Option<DocumentDisplay> {
    let renderer = entry.drawings.renderer.as_ref()?.clone();
    let graphs = mermaid_sources(markdown);
    if graphs.is_empty() {
        return None;
    }
    let mut state = entry.drawings.state.lock().ok()?;
    state.retry.retain(|(_, start)| start.elapsed() < BACKOFF);
    let mut hints = Vec::new();
    for source in graphs {
        let hash = digest(&source);
        let Ok(ready) = renderer.ready_font(&hash) else {
            continue;
        };
        if let Some(ready_font) = ready {
            if hints
                .iter()
                .any(|hint: &DrawingHint| hint.source_sha256.as_str() == hash.as_str())
            {
                continue;
            }
            if let (Some(source_sha256), Some(font)) = (
                ReportHashDto::new(hash.as_str()),
                ReportHashDto::new(ready_font.as_str()),
            ) {
                let locator = Locator {
                    owner: owner.clone(),
                    source_hash: source_sha256.clone(),
                    recipe: DRAWING_RECIPE.to_owned(),
                    font: ready_font.to_string(),
                };
                if let Some(reference) = entry
                    .auth
                    .endpoint
                    .encode_cursor(TOKEN, &locator)
                    .ok()
                    .and_then(PageCursorDto::new)
                {
                    hints.push(DrawingHint {
                        reference,
                        source_sha256,
                        recipe: locator.recipe,
                        font,
                    });
                }
            }
        } else if prepare
            && state.pending.len() < PENDING
            && state.active.as_ref() != Some(&hash)
            && !state.pending.iter().any(|job| job.hash == hash)
            && !state.retry.iter().any(|(previous, _)| previous == &hash)
        {
            state.pending.push_back(Job {
                actor: actor.clone(),
                owner: owner.clone(),
                source,
                hash,
                font: font.cloned(),
            });
        }
    }
    if !state.running && !state.pending.is_empty() {
        state.running = true;
        tokio::spawn(build(entry.clone(), renderer));
    }
    (!hints.is_empty()).then_some(DocumentDisplay {
        drawings: hints,
        table: None,
    })
}

async fn build(entry: Arc<Entry>, renderer: Arc<dyn DrawingRenderer>) {
    let mut shutdown = entry.shutdown.clone();
    loop {
        let Job {
            actor,
            owner,
            source,
            hash,
            font,
        } = {
            let Ok(mut state) = entry.drawings.state.lock() else {
                return;
            };
            let Some(job) = state.pending.pop_front() else {
                state.running = false;
                state.active = None;
                return;
            };
            state.active = Some(job.hash.clone());
            job
        };
        if !renderer.can_draw(font.as_ref()) || !matches!(renderer.ready_font(&hash), Ok(None)) {
            continue;
        }
        let finished = tokio::select! {
            _ = shutdown.changed() => break,
            result = async {
                authorized(&entry, actor.clone(), &owner, hash.as_str()).await?;
                let output = renderer.draw(source).await?;
                authorized(&entry, actor, &owner, hash.as_str()).await?;
                renderer.publish(hash.clone(), output).await
            } => result.is_ok(),
        };
        let Ok(mut state) = entry.drawings.state.lock() else {
            return;
        };
        if !finished {
            if state.retry.len() == BACKOFF_RECORDS {
                state.retry.pop_front();
            }
            state.retry.push_back((hash, Instant::now()));
        }
    }
}

pub(super) async fn read(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(reference): Path<String>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let locator: Locator = entry.auth.endpoint.decode_cursor(TOKEN, &reference)?;
    if locator.recipe != DRAWING_RECIPE {
        return Err(AppError::CursorExpired.into());
    }
    authorized(&entry, actor, &locator.owner, locator.source_hash.as_str()).await?;
    let renderer = entry.drawings.renderer.as_ref().ok_or(AppError::NotFound)?;
    let svg = renderer
        .read(parse(locator.source_hash.as_str())?, parse(&locator.font)?)
        .await?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "image/svg+xml")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'; style-src 'unsafe-inline'",
        )
        .body(Body::from(svg))
        .map_err(|_| AppError::Internal)?)
}

async fn authorized(
    entry: &Entry,
    actor: Actor,
    owner: &Owner,
    hash: &str,
) -> Result<(), AppError> {
    match owner {
        Owner::Report {
            run,
            receipt,
            report_hash,
        } => {
            let value = entry
                .store
                .read_report(actor, parse(run)?, Some(parse(receipt)?), false)
                .await?
                .value;
            if value.report.is_none_or(|report| {
                !report.stats.complete || report.reference.hash.as_str() != report_hash
            }) {
                return Err(AppError::CursorExpired);
            }
        }
        Owner::Native {
            run,
            session,
            source,
            window,
        } => {
            let markdown = if let Some(window) = window {
                drawing_source(
                    entry,
                    actor,
                    parse(run)?,
                    parse(session)?,
                    source,
                    window.byte_start,
                    parse(&window.sha256)?,
                )
                .await?
            } else {
                let reply = read_content(
                    entry,
                    actor,
                    parse(run)?,
                    parse(session)?,
                    ContentQuery {
                        reference: source.clone(),
                        cursor: None,
                    },
                    64 * 1024,
                )
                .await?;
                if !reply.data.complete || reply.data.reset {
                    return Err(AppError::CursorExpired);
                }
                reply.data.text.as_str().to_owned()
            };
            if !mermaid_sources(&markdown)
                .iter()
                .any(|graph| digest(graph).as_str() == hash)
            {
                return Err(AppError::CursorExpired);
            }
        }
    }
    Ok(())
}

pub(super) fn digest(source: &str) -> ReportHash {
    ReportHash::from_sha256(Sha256::digest(source.as_bytes()).into())
}
