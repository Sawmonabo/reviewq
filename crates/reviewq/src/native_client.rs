use std::path::Path;
use std::time::Duration;

use reqwest::Client;
use reviewq_domain::identity::{NativeSessionId, ReviewId, RunId, SessionKey};
use reviewq_transport::{ServiceEndpoint, ServiceIdentity};
use serde::Deserialize;

use crate::cli::Open;
use crate::runtime;

mod claude;
pub(crate) use claude::{ClaudeBridgeOperation, claude_bridge};

type Failure = Box<dyn std::error::Error + Send + Sync>;

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct Ready {
    instance: String,
    ready: bool,
    browser: String,
    native: String,
    #[serde(flatten)]
    identity: ServiceIdentity,
}

#[derive(Deserialize)]
struct Ticket {
    ticket: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Linked {
    review_id: String,
    run_id: String,
    lead_session_key: String,
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
    let endpoint = runtime::read_endpoint(profile)?;
    let client = client()?;
    let status = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &status)?;
    let body = serde_json::to_vec(&serde_json::json!({
        "protocolVersion": 1, "threadId": native.as_str(), "target": target
    }))?;
    let linked =
        link_result(bridge(&client, &endpoint, "/api/v1/native/codex/link", body).await?).await?;
    println!("{}", link_text(&endpoint, linked, "Codex")?);
    Ok(())
}

pub(crate) async fn claude(
    profile: &Path,
    session_id: String,
    target: Option<String>,
) -> Result<(), Failure> {
    let native: NativeSessionId = session_id.parse()?;
    let endpoint = runtime::read_endpoint(profile)?;
    let client = client()?;
    let status = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &status)?;
    let cwd = std::env::current_dir()?.canonicalize()?;
    let home = runtime::claude_home()?.canonicalize()?;
    let body = serde_json::to_vec(&serde_json::json!({
        "protocolVersion": 1,
        "sessionId": native.as_str(),
        "cwd": cwd,
        "profile": home,
        "target": target,
    }))?;
    let (linked, message) = if target.is_some() {
        let linked =
            link_result(bridge(&client, &endpoint, "/api/v1/native/claude/link", body).await?)
                .await?;
        (true, link_text(&endpoint, linked, "Claude Code")?)
    } else {
        #[derive(Deserialize)]
        struct Registration {
            registration: Option<Linked>,
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
            (true, format!("ReviewQ · linked to {}", linked.review_id))
        } else {
            (
                false,
                "ReviewQ · use /reviewq <merge request or ticket> to link this session.".to_owned(),
            )
        }
    };
    println!(
        "{}",
        serde_json::json!({"linked": linked, "message": message})
    );
    Ok(())
}

async fn bridge(
    client: &Client,
    endpoint: &ServiceEndpoint,
    path: &str,
    body: Vec<u8>,
) -> Result<reqwest::Response, Failure> {
    Ok(client
        .post(format!("http://{}{path}", endpoint.native()))
        .headers(endpoint.bridge_headers()?)
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
    linked: Linked,
    provider: &str,
) -> Result<String, Failure> {
    let review: ReviewId = linked.review_id.parse()?;
    let run: RunId = linked.run_id.parse()?;
    let _: SessionKey = linked.lead_session_key.parse()?;
    let mut url = reqwest::Url::parse(&format!("http://{}/", endpoint.browser()))?;
    url.query_pairs_mut()
        .append_pair("reviewId", review.as_str())
        .append_pair("runId", run.as_str())
        .append_pair("tab", "agent");
    Ok(format!(
        "Linked this {provider} conversation.\n{url}\nContinue the review in this terminal or open it with ReviewQ."
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
    verify_ready(&endpoint, &body)?;
    let mut status: serde_json::Value = serde_json::from_slice(&body)?;
    status["data"]["runtimeConfig"] = serde_json::to_value(path)?;
    println!("{status}");
    Ok(())
}

pub(crate) async fn refresh(profile: &Path) -> Result<(), Failure> {
    let endpoint = runtime::read_endpoint(profile)?;
    let client = client()?;
    let body = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &body)?;
    let bytes = signed(&client, &endpoint, "POST", "/api/v1/queue/refresh", b"").await?;
    println!("{}", String::from_utf8(bytes)?);
    Ok(())
}

pub(crate) async fn open(profile: &Path, options: Open) -> Result<(), Failure> {
    let endpoint = runtime::read_endpoint(profile)?;
    let client = client()?;
    let status = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &status)?;
    let bytes = signed(
        &client,
        &endpoint,
        "POST",
        "/api/v1/session",
        br#"{"mode":"launch"}"#,
    )
    .await?;
    let ticket = serde_json::from_slice::<Envelope<Ticket>>(&bytes)?
        .data
        .ticket;
    if ticket.len() != 43
        || !ticket
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("invalid browser ticket".into());
    }
    let url = format!("http://{}/#ticket={ticket}", endpoint.browser());
    if options.print_url {
        println!("{url}");
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
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
    let bytes = bounded(response, 8192).await?;
    endpoint.verify_response(&proof, status.as_u16(), &headers, &bytes)?;
    if !status.is_success() {
        return Err("service rejected the operation".into());
    }
    Ok(bytes)
}

pub(crate) fn verify_ready(endpoint: &ServiceEndpoint, bytes: &[u8]) -> Result<(), Failure> {
    let ready = serde_json::from_slice::<Envelope<Ready>>(bytes)?.data;
    if ready.instance != endpoint.instance()
        || ready.identity != *endpoint.identity()
        || ready.browser != endpoint.browser().to_string()
        || ready.native != endpoint.native().to_string()
        || !ready.ready
    {
        return Err("service instance is not ready".into());
    }
    Ok(())
}

async fn bounded(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>, Failure> {
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
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(30))
        .build()?)
}
