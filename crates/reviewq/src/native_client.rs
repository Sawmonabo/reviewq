use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::Client;
use reviewq_domain::identity::{NativeSessionId, ReviewId, RunId, SessionKey};
use reviewq_domain::session::HostProvider;
use reviewq_protocol::EventCursorDto;
use reviewq_providers::open_windows_browser;
use reviewq_transport::{ServiceEndpoint, ServiceIdentity};
use serde::Deserialize;

use crate::cli::Open;
use crate::runtime;

mod claude;
mod codex_hook;
mod hook;
pub(crate) use claude::{ClaudeBridgeOperation, claude_bridge, claude_hook};
pub(crate) use codex_hook::codex_hook;

type Failure = Box<dyn std::error::Error + Send + Sync>;

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct Ready {
    instance: String,
    browser: String,
    native: String,
    #[serde(flatten)]
    state: Readiness,
    #[serde(flatten)]
    identity: ServiceIdentity,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Readiness {
    pub(crate) ready: bool,
    pub(crate) recovery_complete: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Linked {
    review_id: String,
    run_id: String,
    lead_session_key: String,
    /// The rule the model reads after linking, absent from a reply that carries none.
    #[serde(default)]
    rule: Option<String>,
    /// Whether a Codex `Stop` hook has reported on this installation; only a Codex link
    /// reply carries it.
    #[serde(default)]
    stop_hook_reported: bool,
}

#[derive(Deserialize)]
struct AmbiguousTarget {
    candidates: Vec<String>,
}

pub(crate) async fn link(profile: &Path, target: String) -> Result<(), Failure> {
    let native: NativeSessionId = std::env::var("CODEX_THREAD_ID")
        .ok()
        .and_then(|id| id.parse().ok())
        .ok_or("Run reviewq link from inside the Codex conversation you want to link.")?;
    let endpoint = runtime::read_native_endpoint(profile)?;
    let client = client()?;
    let body = serde_json::to_vec(&serde_json::json!({
        "protocolVersion": 1, "threadId": native.as_str(), "target": target
    }))?;
    let linked =
        link_result(bridge(&client, &endpoint, "/api/v1/native/codex/link", body).await?).await?;
    println!("{}", link_text(&endpoint, &linked, HostProvider::Codex)?);
    if let Some(rule) = &linked.rule {
        println!("\n{rule}");
    }
    if !linked.stop_hook_reported {
        println!("\n{CODEX_HOOKS_PENDING}");
    }
    Ok(())
}

/// Shown by `reviewq link` and the installer while no Codex `Stop` hook has reported on this
/// installation.
pub(crate) const CODEX_HOOKS_PENDING: &str = "The ReviewQ hooks have not run in Codex yet: open /hooks in Codex to review and trust them. Until then a review still files, closes and shows as unfinished when it is not closed; only the end-of-turn reminder and the reminder after compaction are missing.";

/// Whether the service behind `endpoint` has never had a Codex `Stop` hook report, so the
/// installer still shows `CODEX_HOOKS_PENDING`. A service that cannot say counts as never.
pub(crate) async fn codex_hooks_pending(endpoint: &ServiceEndpoint) -> bool {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CodexHooks {
        stop_hook_reported: bool,
    }
    let reported = async {
        let body = signed(
            &client()?,
            endpoint,
            "GET",
            "/api/v1/control/codex-hooks",
            b"",
        )
        .await?;
        Ok::<_, Failure>(
            serde_json::from_slice::<Envelope<CodexHooks>>(&body)?
                .data
                .stop_hook_reported,
        )
    };
    !reported.await.unwrap_or(false)
}

pub(crate) async fn claude(
    profile: &Path,
    session_id: String,
    target: Option<String>,
    proof_stdin: bool,
    transcript: Option<PathBuf>,
) -> Result<(), Failure> {
    let native: NativeSessionId = session_id.parse()?;
    let proof = if proof_stdin {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Proof {
            proof: String,
        }
        Some(serde_json::from_slice::<Proof>(&read_input(1024)?)?.proof)
    } else {
        None
    };
    let endpoint = runtime::read_native_endpoint(profile)?;
    let client = client()?;
    let cwd = std::env::current_dir()?.canonicalize()?;
    let home = runtime::claude_home()?.canonicalize()?;
    let body = serde_json::to_vec(&serde_json::json!({
        "protocolVersion": 1,
        "sessionId": native.as_str(),
        "cwd": cwd,
        "profile": home,
        "transcript": transcript,
        "target": target,
    }))?;
    let (linked, pending_launch, message, rule) = if target.is_some() {
        // A link of a session whose companion is live must carry that companion's proof.
        let headers = match &proof {
            Some(proof) => endpoint.companion_headers(proof)?,
            None => endpoint.bridge_headers()?,
        };
        let linked = link_result(
            bridge_as(
                &client,
                &endpoint,
                "/api/v1/native/claude/link",
                body,
                headers,
            )
            .await?,
        )
        .await?;
        (
            true,
            false,
            link_text(&endpoint, &linked, HostProvider::Claude)?,
            linked.rule,
        )
    } else {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Registration {
            registration: Option<Linked>,
            pending_launch: bool,
        }
        let response = bridge(
            &client,
            &endpoint,
            "/api/v1/native/claude/registration",
            body,
        )
        .await?;
        let status = response.status();
        let bytes = bounded(response, 16 * 1024).await?;
        if !status.is_success() {
            return Err("ReviewQ could not verify this Claude Code session.".into());
        }
        let data = serde_json::from_slice::<Envelope<Registration>>(&bytes)?.data;
        if let Some(linked) = data.registration {
            (
                true,
                false,
                format!("ReviewQ · linked to {}", linked.review_id),
                None,
            )
        } else if data.pending_launch {
            (
                false,
                true,
                "ReviewQ is confirming this managed Claude launch.".to_owned(),
                None,
            )
        } else {
            (
                false,
                false,
                "ReviewQ · use /reviewq <merge request or ticket> to link this session.".to_owned(),
                None,
            )
        }
    };
    println!(
        "{}",
        serde_json::json!({
            "linked": linked, "pendingLaunch": pending_launch, "message": message, "rule": rule,
        })
    );
    Ok(())
}

/// Posts `body` to the native route `path` with the bridge credential, once a signed status
/// request has proved that the listener is this installation's service instance and that it
/// is ready. The credential is never sent, and no reply is read, without that proof. Fails
/// without posting when the status reply is unsigned, names another instance or reports a
/// service still starting, and fails when either request fails.
pub(crate) async fn bridge(
    client: &Client,
    endpoint: &ServiceEndpoint,
    path: &str,
    body: Vec<u8>,
) -> Result<reqwest::Response, Failure> {
    bridge_as(client, endpoint, path, body, endpoint.bridge_headers()?).await
}

/// `bridge` with `headers` in place of the bare bridge credential, which they include.
pub(crate) async fn bridge_as(
    client: &Client,
    endpoint: &ServiceEndpoint,
    path: &str,
    body: Vec<u8>,
    headers: reqwest::header::HeaderMap,
) -> Result<reqwest::Response, Failure> {
    let status = signed(client, endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(endpoint, &status)?;
    Ok(client
        .post(format!("http://{}{path}", endpoint.native()))
        .headers(headers)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await?)
}

async fn link_result(response: reqwest::Response) -> Result<Linked, Failure> {
    let status = response.status();
    let bytes = bounded(response, 16 * 1024).await?;
    if !status.is_success() {
        let message = match status.as_u16() {
            401 => "ReviewQ did not accept the credential for this link.",
            404 => "The native conversation or requested review could not be verified.",
            409 => {
                if let Ok(ambiguity) = serde_json::from_slice::<AmbiguousTarget>(&bytes) {
                    return Err(format!(
                        "Choose one of these reviews and link its URL.\n{}",
                        ambiguity.candidates.join("\n")
                    )
                    .into());
                }
                "This conversation is already linked to a different review."
            }
            412 => "Reload the ReviewQ integration in this native client and try again.",
            _ => "The service could not link the current native conversation.",
        };
        return Err(message.into());
    }
    Ok(serde_json::from_slice::<Envelope<Linked>>(&bytes)?.data)
}

fn link_text(
    endpoint: &ServiceEndpoint,
    linked: &Linked,
    provider: HostProvider,
) -> Result<String, Failure> {
    let review: ReviewId = linked.review_id.parse()?;
    let run: RunId = linked.run_id.parse()?;
    let _: SessionKey = linked.lead_session_key.parse()?;
    let mut url = reqwest::Url::parse(&format!("http://{}/", endpoint.browser()))?;
    url.query_pairs_mut()
        .append_pair("reviewId", review.as_str())
        .append_pair("runId", run.as_str())
        .append_pair("tab", "agent");
    let name = match provider {
        HostProvider::Claude => "Claude Code",
        HostProvider::Codex => "Codex",
    };
    Ok(format!(
        "Linked this {name} conversation.\n{url}\nContinue the review in this terminal or open it with ReviewQ."
    ))
}

pub(crate) async fn status(profile: &Path) -> Result<(), Failure> {
    let path = crate::runtime_settings::config_path(profile, false);
    if !runtime::has_descriptor(profile)? {
        println!(
            "{}",
            serde_json::json!({"data": {"ready": false, "runtimeConfig": path}})
        );
        return Ok(());
    }
    let endpoint = runtime::read_endpoint(profile)?;
    let body = signed(&client()?, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_service(&endpoint, &body)?;
    let mut status: serde_json::Value = serde_json::from_slice(&body)?;
    status["data"]["runtimeConfig"] = serde_json::to_value(path)?;
    println!("{status}");
    Ok(())
}

pub(crate) async fn refresh(profile: &Path) -> Result<(), Failure> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Refresh {
        refresh_id: EventCursorDto,
        state: RefreshState,
    }
    #[derive(Deserialize, Eq, PartialEq)]
    #[serde(rename_all = "snake_case")]
    enum RefreshState {
        Running,
        Succeeded,
        Failed,
        Superseded,
    }

    let endpoint = runtime::read_endpoint(profile)?;
    let client = client()?;
    let body = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &body)?;
    let mut bytes = signed(&client, &endpoint, "POST", "/api/v1/queue/refresh", b"").await?;
    let mut receipt = serde_json::from_slice::<Envelope<Refresh>>(&bytes)?.data;
    let id = receipt.refresh_id;
    let path = format!("/api/v1/queue/refresh/{}", id.get());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    while receipt.state == RefreshState::Running && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(750)).await;
        bytes = signed(&client, &endpoint, "GET", &path, b"")
            .await
            .map_err(|error| {
                format!(
                    "{error}. Read refresh {} to inspect the recorded outcome.",
                    id.get()
                )
            })?;
        receipt = serde_json::from_slice::<Envelope<Refresh>>(&bytes)?.data;
        if receipt.refresh_id != id {
            return Err("The service returned a different refresh ID.".into());
        }
    }
    println!("{}", String::from_utf8(bytes)?);
    if receipt.state != RefreshState::Succeeded {
        return Err(format!("Refresh {} has no successful publication.", id.get()).into());
    }
    Ok(())
}

pub(crate) async fn open(profile: &Path, options: Open) -> Result<(), Failure> {
    let endpoint = runtime::read_endpoint(profile)?;
    let client = client()?;
    let status = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &status)?;
    let url = reqwest::Url::parse(&format!("http://{}/", endpoint.browser()))?;
    if options.print_url {
        println!("{url}");
        return Ok(());
    }
    let opened = match open_windows_browser(&url).await {
        Some(opened) => opened,
        None => crate::service::capture("xdg-open", &[url.as_str()], true)
            .await
            .is_ok_and(|(opened, _)| opened),
    };
    if !opened {
        println!("Open ReviewQ at {url}");
    }
    Ok(())
}

pub(crate) async fn signed(
    client: &Client,
    endpoint: &ServiceEndpoint,
    method: &str,
    target: &str,
    body: &[u8],
) -> Result<Vec<u8>, Failure> {
    signed_limit(client, endpoint, method, target, body, 8192).await
}

pub(crate) async fn signed_limit(
    client: &Client,
    endpoint: &ServiceEndpoint,
    method: &str,
    target: &str,
    body: &[u8],
    limit: usize,
) -> Result<Vec<u8>, Failure> {
    let proof = endpoint.sign(method, target, body)?;
    let mut request = client
        .request(
            method.parse()?,
            format!("http://{}{}", endpoint.native(), target),
        )
        .headers(proof.headers.clone());
    if !body.is_empty() {
        request = request
            .header("content-type", "application/json")
            .body(body.to_vec());
    }
    let response = request.send().await?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = bounded(response, limit).await?;
    endpoint.verify_response(&proof, status.as_u16(), &headers, &bytes)?;
    if !status.is_success() {
        if let Ok(failure) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some(code) = failure
                .pointer("/error/code")
                .and_then(serde_json::Value::as_str)
            && code.len() <= 64
            && code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            return Err(format!("ReviewQ returned {code}").into());
        }
        return Err("ReviewQ rejected the operation".into());
    }
    Ok(bytes)
}

pub(crate) fn verify_ready(endpoint: &ServiceEndpoint, bytes: &[u8]) -> Result<(), Failure> {
    if !verify_service(endpoint, bytes)?.ready {
        return Err("service instance is still starting".into());
    }
    Ok(())
}

pub(crate) fn verify_service(
    endpoint: &ServiceEndpoint,
    bytes: &[u8],
) -> Result<Readiness, Failure> {
    let ready = serde_json::from_slice::<Envelope<Ready>>(bytes)?.data;
    if ready.instance != endpoint.instance()
        || ready.identity != *endpoint.identity()
        || ready.browser != endpoint.browser().to_string()
        || ready.native != endpoint.native().to_string()
    {
        return Err("service response identifies a different instance".into());
    }
    Ok(ready.state)
}

/// Reads all of stdin, refusing more than `limit` bytes.
fn read_input(limit: usize) -> Result<Vec<u8>, Failure> {
    read_within(limit)?.ok_or_else(|| "ReviewQ companion input exceeds its limit.".into())
}

/// Reads all of stdin, or returns `None` when it holds more than `limit` bytes. Past the
/// limit the rest is read and discarded, so memory stays within `limit` and the writer never
/// meets a closed pipe.
fn read_within(limit: usize) -> Result<Option<Vec<u8>>, Failure> {
    let mut stdin = std::io::stdin().lock();
    let mut input = Vec::new();
    (&mut stdin)
        .take((limit + 1) as u64)
        .read_to_end(&mut input)?;
    if input.len() > limit {
        std::io::copy(&mut stdin, &mut std::io::sink())?;
        return Ok(None);
    }
    Ok(Some(input))
}

pub(crate) async fn bounded(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > max - bytes.len() {
            return Err("service response exceeds limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) fn client() -> Result<Client, Failure> {
    client_with_timeout(Some(Duration::from_secs(30)))
}

fn client_with_timeout(timeout: Option<Duration>) -> Result<Client, Failure> {
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2));
    let client = match timeout {
        Some(timeout) => client.timeout(timeout),
        None => client,
    };
    Ok(client.build()?)
}
