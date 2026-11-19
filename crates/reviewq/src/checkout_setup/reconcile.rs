use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reviewq_app::access::Actor;
use reviewq_app::workspace::{
    CheckoutProjection, IssueGrant, ProjectionOutcome, RepositoryChatProjectionStore,
    WorkspaceStore,
};
use reviewq_store::SqliteStore;
use sha2::{Digest, Sha256};

use super::{Failure, clean_owned, files, git, prepare, projection};

pub(crate) async fn run(
    store: Arc<SqliteStore>,
    profile: PathBuf,
    endpoint: std::net::SocketAddr,
    claude_profile: Option<PathBuf>,
) {
    let mut commits = store.subscribe_checkout_projections();
    let url = format!("http://{endpoint}/mcp");
    let mut boot = true;
    loop {
        commits.borrow_and_update();
        if reconcile(
            store.as_ref(),
            &profile,
            &url,
            claude_profile.as_deref(),
            boot,
        )
        .await
        .is_err()
        {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => (),
                result = commits.changed() => if result.is_err() { break; },
            }
            continue;
        }
        boot = false;
        if commits.changed().await.is_err() {
            break;
        }
    }
}

async fn reconcile(
    store: &SqliteStore,
    profile: &Path,
    url: &str,
    claude_profile: Option<&Path>,
    boot: bool,
) -> Result<(), Failure> {
    let mut after = None;
    loop {
        let jobs = store.checkout_projections(boot, after).await?;
        let Some(last) = jobs.last() else {
            return Ok(());
        };
        after = Some(last.checkout.common_dir.clone());
        for job in jobs {
            if let Err(error) = one(store, profile, url, claude_profile, &job).await {
                let issue = format!("{}: {error}", job.checkout.root.as_path().display());
                let issue = issue
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .scan(0, |size, ch| {
                        *size += ch.len_utf8();
                        (*size <= 1024).then_some(ch)
                    })
                    .collect::<String>();
                let _ = store
                    .finish_checkout_projection(job, ProjectionOutcome::Deferred(issue))
                    .await;
            }
        }
    }
}

async fn one(
    store: &SqliteStore,
    profile: &Path,
    url: &str,
    claude_profile: Option<&Path>,
    job: &CheckoutProjection,
) -> Result<(), Failure> {
    let common = job.checkout.common_dir.as_path();
    let lock = files::hold(profile, common)?;
    store.check_checkout_projection(job.clone()).await?;
    if !job.desired {
        clean_owned(profile, common).await?;
        store
            .finish_checkout_projection(job.clone(), ProjectionOutcome::Removed)
            .await?;
        return Ok(());
    }
    let checkout = git::discover(Some(job.checkout.root.as_path())).await?;
    if checkout.root != job.checkout.root.as_path() || checkout.common != common {
        return Err("The local Git checkout has changed. Update its binding in Settings.".into());
    }
    if let Some(key) = files::existing_credential(profile, common)? {
        if store
            .retired_checkout_verifier(job.clone(), Sha256::digest(key).into())
            .await?
        {
            clean_owned(profile, common).await?;
        }
    }
    let claude = projection::claude_config(claude_profile)?;
    let mut changes = prepare(profile, checkout, url, claude, lock).await?;
    store
        .issue_grant(
            Actor::local_operator(),
            IssueGrant {
                settings: job.settings,
                project: job.project.clone(),
                checkout: job.checkout.root.clone(),
                common_dir: job.checkout.common_dir.clone(),
                verifier: changes.verifier,
                operator_requested: false,
            },
        )
        .await?;
    changes.apply().await?;
    if let Err(error) = store
        .finish_checkout_projection(job.clone(), ProjectionOutcome::Applied(changes.verifier))
        .await
    {
        changes.plan.rollback()?;
        return Err(error.into());
    }
    changes.finish()?;
    Ok(())
}
