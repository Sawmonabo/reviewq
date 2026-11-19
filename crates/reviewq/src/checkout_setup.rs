use std::collections::BTreeSet;
use std::fs::File;
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
mod hooks;
mod observe;
mod projection;
pub(crate) mod reconcile;

type Failure = Box<dyn std::error::Error + Send + Sync>;
const GRANT: &str = "/api/v1/checkout/grant";

pub(crate) async fn worktree(
    profile: &Path,
    settings: PathBuf,
    common: PathBuf,
) -> Result<(), Failure> {
    if settings != crate::runtime_settings::config_path(profile, false)
        || git::common_directory(None).await? != common
    {
        return Err("The Git checkout does not match the registered ReviewQ setup.".into());
    }
    let worktree = git::current_worktree().await?;
    let inventory = projection::Inventory::load(&files::inventory(profile, &common)?)?;
    if inventory.has_worktree(&worktree).await? {
        return Ok(());
    }
    let endpoint = runtime::read_endpoint(profile)?;
    let client = native_client::client()?;
    let settings_revision = settings_revision(&client, &endpoint).await?;
    let fingerprint = observe::Topology::read(&common)?.fingerprint;
    let body = serde_json::to_vec(&json!({ "action":"projectWorktree", "commonDir":common,
        "settingsRevision":settings_revision, "topology":URL_SAFE_NO_PAD.encode(fingerprint) }))?;
    native_client::signed(&client, &endpoint, "POST", GRANT, &body).await?;
    Ok(())
}

pub(crate) async fn install(
    profile: &Path,
    directory: Option<PathBuf>,
    project: ProjectId,
    claude_home: Option<PathBuf>,
) -> Result<(), Failure> {
    let checkout = git::discover(directory.as_deref()).await?;
    let endpoint = runtime::read_endpoint(profile)?;
    let client = native_client::client()?;
    let revision = settings_revision(&client, &endpoint).await?;
    let url = format!("http://{}/mcp", endpoint.native());
    let claude = projection::claude_config(claude_home.as_deref())?;
    let lock = files::hold(profile, &checkout.common)?;
    let mut changes = prepare(profile, checkout, &url, claude, lock).await?;
    let address = json!({
        "settingsRevision":revision, "projectId":project.to_string(),
        "checkout":changes.checkout.root, "commonDir":changes.checkout.common,
        "verifier":URL_SAFE_NO_PAD.encode(changes.verifier),
    });
    let request = serde_json::to_vec(&json!({"action":"issue","address":address}))?;
    let confirm = serde_json::to_vec(&json!({"action":"status","address":address}))?;
    let applied = serde_json::to_vec(&json!({"action":"applied","address":address}))?;
    let issued = native_client::signed(&client, &endpoint, "POST", GRANT, &request).await;
    if let Ok(bytes) = issued {
        verify_binding(&bytes, &changes.checkout, project, revision)?;
    }
    let bytes=native_client::signed(&client,&endpoint,"POST",GRANT,&confirm).await
        .map_err(|_|"ReviewQ could not confirm management access for this exact checkout and settings revision.")?;
    verify_binding(&bytes, &changes.checkout, project, revision)?;
    changes.apply().await?;
    match native_client::signed(&client, &endpoint, "POST", GRANT, &applied)
        .await
        .and_then(|bytes| verify_binding(&bytes, &changes.checkout, project, revision))
    {
        Ok(()) => (),
        Err(_) => {
            changes.plan.rollback()?;
            return Err("Checkout authority changed during setup.".into());
        }
    }
    changes.finish()?;
    println!("ReviewQ management access is ready for this checkout.");
    Ok(())
}

async fn settings_revision(
    client: &reqwest::Client,
    endpoint: &reviewq_transport::ServiceEndpoint,
) -> Result<RevisionDto, Failure> {
    let status = native_client::signed(client, endpoint, "GET", "/api/v1/status", b"").await?;
    native_client::verify_ready(endpoint, &status)?;
    let status: Value = serde_json::from_slice(&status)?;
    Ok(serde_json::from_value(
        status
            .pointer("/data/settingsRevision")
            .cloned()
            .ok_or("The service did not return a settings revision.")?,
    )?)
}

struct Prepared {
    profile: PathBuf,
    checkout: git::Checkout,
    claude: Option<PathBuf>,
    inventory: PathBuf,
    plan: projection::Plan,
    verifier: [u8; 32],
    _lock: File,
    _client_locks: Vec<File>,
}

impl Prepared {
    async fn apply(&mut self) -> Result<(), Failure> {
        self.plan
            .apply(&self.inventory, &self.checkout, self.claude.as_deref())
            .await?;
        match hooks::reconcile(&self.profile, &self.checkout).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.plan.rollback()?;
                Err(error)
            }
        }
    }

    fn finish(self) -> Result<(), Failure> {
        self.plan.finish(&self.inventory)
    }
}

async fn prepare(
    profile: &Path,
    checkout: git::Checkout,
    url: &str,
    claude: Option<PathBuf>,
    lock: File,
) -> Result<Prepared, Failure> {
    projection::require_untracked(&checkout, claude.as_deref()).await?;
    let key = files::credential(profile, &checkout.common)?;
    let inventory = files::inventory(profile, &checkout.common)?;
    let previous = projection::Inventory::load(&inventory)?;
    let client_locks = lock_clients(profile, &previous, claude.as_deref())?;
    let plan = projection::Plan::new(
        &checkout,
        url,
        &URL_SAFE_NO_PAD.encode(key),
        claude.as_deref(),
        previous,
    )
    .await?;
    Ok(Prepared {
        profile: profile.to_owned(),
        checkout,
        claude,
        inventory,
        plan,
        verifier: Sha256::digest(key).into(),
        _lock: lock,
        _client_locks: client_locks,
    })
}

fn lock_clients(
    profile: &Path,
    previous: &projection::Inventory,
    claude: Option<&Path>,
) -> Result<Vec<File>, Failure> {
    previous
        .claude_files()
        .chain(claude)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|path| files::hold_client(profile, path))
        .collect()
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
        || bound.checkout != checkout.root
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
    clean_owned(profile, &common).await?;
    println!("ReviewQ management access was revoked for this repository.");
    Ok(())
}

async fn clean_owned(profile: &Path, common: &Path) -> Result<(), Failure> {
    let hooks = hooks::cleanup(profile, common);
    let inventory = files::inventory(profile, common)?;
    let previous = projection::Inventory::load(&inventory)?;
    let _client_locks = lock_clients(profile, &previous, None)?;
    let key = files::existing_credential(profile, common)?;
    previous
        .cleanup(
            &inventory,
            common,
            &key.map_or_else(String::new, |key| URL_SAFE_NO_PAD.encode(key)),
        )
        .await?;
    files::forget(profile, common)?;
    hooks
}
