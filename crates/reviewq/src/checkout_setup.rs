use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_domain::identity::ProjectId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{native_client, runtime};

mod files;
mod git;
mod projection;

type Failure = Box<dyn std::error::Error + Send + Sync>;
const GRANT: &str = "/api/v1/checkout/grant";

pub(crate) async fn install(
    profile: &Path,
    directory: Option<PathBuf>,
    project: ProjectId,
    claude_home: Option<PathBuf>,
) -> Result<(), Failure> {
    let checkout = git::discover(directory.as_deref()).await?;
    let endpoint = runtime::read_endpoint(profile)?;
    let client = native_client::client()?;
    let status = native_client::signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    native_client::verify_ready(&endpoint, &status)?;
    let _lock = files::hold(profile, &checkout.common)?;
    let key = files::credential(profile, &checkout.common)?;
    let bearer = URL_SAFE_NO_PAD.encode(key);
    let verifier = URL_SAFE_NO_PAD.encode(Sha256::digest(key));
    let url = format!("http://{}/mcp", endpoint.native());

    let mut changes = projection::checkout(&checkout, &url, &bearer, claude_home.as_deref())?;
    changes.apply()?;
    let request = serde_json::to_vec(&json!({
        "action":"issue", "projectId":project.to_string(), "checkout":checkout.primary,
        "commonDir":checkout.common, "remote":checkout.remote, "verifier":verifier,
    }))?;
    match native_client::signed(&client, &endpoint, "POST", GRANT, &request).await {
        Ok(_) => {}
        Err(_) => {
            let value = serde_json::to_vec(&json!({"action":"status", "verifier":verifier}))?;
            match native_client::signed(&client, &endpoint, "POST", GRANT, &value).await {
                Ok(answer) => {
                    let result: Value = serde_json::from_slice(&answer)?;
                    match result.pointer("/data/active").and_then(Value::as_bool) {
                        Some(true) => {}
                        Some(false) => {
                            changes.rollback()?;
                            return Err(
                                "ReviewQ could not grant management access to this checkout."
                                    .into(),
                            );
                        }
                        None => {
                            return Err(
                                "Checkout files were staged, but grant status is unknown.".into()
                            );
                        }
                    }
                }
                Err(_) => {
                    return Err("Checkout files were staged, but grant status is unknown.".into());
                }
            }
        }
    };
    println!("ReviewQ management access is ready for this checkout.");
    Ok(())
}

pub(crate) async fn revoke(profile: &Path, directory: Option<PathBuf>) -> Result<(), Failure> {
    let common = git::common_directory(directory.as_deref()).await?;
    let endpoint = runtime::read_endpoint(profile)?;
    let client = native_client::client()?;
    let status = native_client::signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    native_client::verify_ready(&endpoint, &status)?;
    let _lock = files::hold(profile, &common)?;
    let body = serde_json::to_vec(&json!({"action":"revoke","commonDir":common}))?;
    native_client::signed(&client, &endpoint, "POST", GRANT, &body).await?;
    files::forget(profile, &common)?;
    println!("ReviewQ management access was revoked for this repository.");
    Ok(())
}
