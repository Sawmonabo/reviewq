use std::path::Path;
use std::time::Duration;

use reqwest::Client;
use reviewq_transport::{ServiceEndpoint, ServiceIdentity};
use serde::Deserialize;

use crate::cli::Open;
use crate::runtime;

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

pub(crate) async fn status(profile: &Path) -> Result<(), Failure> {
    let endpoint = runtime::read_endpoint(profile)?;
    let body = signed(&client()?, &endpoint, "GET", "/api/v1/status", b"").await?;
    verify_ready(&endpoint, &body)?;
    println!("{}", String::from_utf8(body)?);
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

async fn signed(
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

fn verify_ready(endpoint: &ServiceEndpoint, bytes: &[u8]) -> Result<(), Failure> {
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

fn client() -> Result<Client, Failure> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(30))
        .build()?)
}
