use std::collections::HashSet;
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

use super::observe::{Observers, Topology};
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
    let mut retry = Duration::from_secs(15);
    let mut observations = Observers::new();
    let wake = observations.wake.clone();
    let mut missed_events = tokio::time::interval(Duration::from_secs(30));
    missed_events.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        commits.borrow_and_update();
        let pending = reconcile(
            store.as_ref(),
            &profile,
            &url,
            claude_profile.as_deref(),
            boot,
            &mut observations,
        )
        .await;
        let pending = match pending {
            Ok(pending) => {
                boot = false;
                pending
            }
            Err(_) => true,
        };
        let retry_at = tokio::time::sleep(retry);
        tokio::pin!(retry_at);
        loop {
            tokio::select! {
                result = commits.changed() => {
                    if result.is_err() { return; }
                    retry = Duration::from_secs(15);
                    break;
                },
                _ = &mut retry_at, if pending => {
                    retry = (retry * 2).min(Duration::from_secs(300));
                    break;
                },
                _ = wake.notified(), if !observations.is_empty() => observations.refresh(store.as_ref()).await,
                _ = missed_events.tick(), if !observations.is_empty() => observations.refresh(store.as_ref()).await,
            }
        }
    }
}

async fn reconcile(
    store: &SqliteStore,
    profile: &Path,
    url: &str,
    claude_profile: Option<&Path>,
    boot: bool,
    observations: &mut Observers,
) -> Result<bool, Failure> {
    let mut after = None;
    let mut pending = false;
    let mut wanted = HashSet::new();
    loop {
        let jobs = store.checkout_projections(after).await?;
        let Some(last) = jobs.last() else {
            observations.retain(&wanted);
            return Ok(pending);
        };
        after = Some(last.checkout.common_dir.clone());
        for job in jobs {
            if job.desired {
                wanted.insert(job.checkout.common_dir.clone());
            }
            if !boot && job.ready {
                continue;
            }
            match one(store, profile, url, claude_profile, &job).await {
                Ok(topology) => observations.record(job.checkout.common_dir, topology),
                Err(error) => {
                    pending = true;
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
}

async fn one(
    store: &SqliteStore,
    profile: &Path,
    url: &str,
    claude_profile: Option<&Path>,
    job: &CheckoutProjection,
) -> Result<Option<Topology>, Failure> {
    let common = job.checkout.common_dir.as_path();
    let lock = files::hold(profile, common)?;
    store.check_checkout_projection(job.clone()).await?;
    if !job.desired {
        clean_owned(profile, common).await?;
        store
            .finish_checkout_projection(job.clone(), ProjectionOutcome::Removed)
            .await?;
        return Ok(None);
    }
    let topology = Topology::read(common)?;
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
    let observe = changes.apply().await?;
    if let Err(error) = store
        .finish_checkout_projection(job.clone(), ProjectionOutcome::Applied(changes.verifier))
        .await
    {
        changes.plan.rollback()?;
        return Err(error.into());
    }
    changes.finish()?;
    Ok(observe.then_some(topology))
}
