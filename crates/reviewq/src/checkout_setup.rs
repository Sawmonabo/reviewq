use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_domain::identity::{GitLabOrigin, ProjectId};
use reviewq_protocol::RevisionDto;
use serde::Deserialize;
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
    let status: Value = serde_json::from_slice(&status)?;
    let revision = serde_json::from_value::<RevisionDto>(
        status
            .pointer("/data/settingsRevision")
            .cloned()
            .ok_or("The service did not return a settings revision.")?,
    )?;
    let _lock = files::hold(profile, &checkout.common)?;
    let key = files::credential(profile, &checkout.common)?;
    let bearer = URL_SAFE_NO_PAD.encode(key);
    let verifier = URL_SAFE_NO_PAD.encode(Sha256::digest(key));
    let url = format!("http://{}/mcp", endpoint.native());

    let mut changes = projection::checkout(&checkout, &url, &bearer, claude_home.as_deref())?;
    let address = json!({
        "settingsRevision":revision, "projectId":project.to_string(),
        "checkout":checkout.primary, "commonDir":checkout.common, "verifier":verifier,
    });
    let request = serde_json::to_vec(&json!({"action":"issue","address":address}))?;
    let confirm = serde_json::to_vec(&json!({"action":"status","address":address}))?;
    let issued = native_client::signed(&client, &endpoint, "POST", GRANT, &request).await;
    if let Ok(bytes) = issued {
        verify_binding(&bytes, &checkout, project, revision)?;
    }
    let bytes=native_client::signed(&client,&endpoint,"POST",GRANT,&confirm).await
        .map_err(|_|"ReviewQ could not confirm management access for this exact checkout and settings revision.")?;
    verify_binding(&bytes, &checkout, project, revision)?;
    changes.apply()?;
    match native_client::signed(&client, &endpoint, "POST", GRANT, &confirm)
        .await
        .and_then(|bytes| verify_binding(&bytes, &checkout, project, revision))
    {
        Ok(()) => (),
        Err(_) => {
            changes.rollback()?;
            return Err("Checkout authority changed during setup.".into());
        }
    }
    println!("ReviewQ management access is ready for this checkout.");
    Ok(())
}

fn verify_binding(
    bytes: &[u8],
    checkout: &git::Checkout,
    project: ProjectId,
    revision: RevisionDto,
) -> Result<(), Failure> {
    #[derive(Deserialize)]
    struct Envelope {
        data: Binding,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Binding {
        grant_id: String,
        epoch: RevisionDto,
        settings_revision: RevisionDto,
        origin: String,
        project_id: String,
        checkout: PathBuf,
        common_dir: PathBuf,
    }
    let bound = serde_json::from_slice::<Envelope>(bytes)?.data;
    if bound.grant_id.is_empty()
        || bound.epoch.get() == 0
        || bound.settings_revision != revision
        || GitLabOrigin::from_canonical(&bound.origin).is_err()
        || bound.project_id != project.to_string()
        || bound.checkout != checkout.primary
        || bound.common_dir != checkout.common
    {
        return Err("ReviewQ returned a different checkout binding.".into());
    }
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
