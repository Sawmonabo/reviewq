use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reviewq_app::access::Actor;
use reviewq_app::workspace::{
    CheckoutProjection, IssueGrant, ProjectionOutcome, RepositoryChatProjectionStore,
    WorkspaceStore,
};
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::text::clip;
use reviewq_store::SqliteStore;
use sha2::{Digest, Sha256};

use super::observe::{Observers, Topology};
use super::{Failure, SERVICE_AGAIN, Unfinished, clean_owned, files, git, prepare, projection};

const ISSUE_LIMIT: usize = 1024;

pub(crate) async fn run(
    store: Arc<SqliteStore>,
    profile: PathBuf,
    url: String,
    claude_profile: Option<PathBuf>,
) {
    let mut commits = store.subscribe_checkout_projections();
    let mut boot = true;
    // Until each checkout only `reviewq checkout setup` set up has the current address.
    let mut command_line = true;
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
            (boot, &mut command_line),
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
    (boot, command_line): (bool, &mut bool),
    observations: &mut Observers,
) -> Result<bool, Failure> {
    let mut after = None;
    let mut pending = false;
    let mut wanted = HashSet::new();
    let mut listed = HashSet::new();
    loop {
        let jobs = store.checkout_projections(after).await?;
        let Some(last) = jobs.last() else {
            observations.retain(&wanted);
            if *command_line {
                *command_line = command_line_setups(store, profile, url, &listed).await?;
            }
            return Ok(pending || *command_line);
        };
        after = Some(last.checkout.common_dir.clone());
        for job in jobs {
            listed.insert(job.checkout.common_dir.clone());
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
                    // The store keeps 1,024 bytes. What setup left keeps its closing step, and
                    // the parts that do not fit are counted.
                    let lead = format!("{}: ", job.checkout.root.as_path().display());
                    let issue = match error.downcast_ref::<Unfinished>() {
                        Some(left) => left.render(&lead, Some(ISSUE_LIMIT)),
                        None => clip(
                            format!("{lead}{error}")
                                .chars()
                                .filter(|ch| !ch.is_control())
                                .collect(),
                            ISSUE_LIMIT,
                        ),
                    };
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
        clean_owned(profile, common, super::SERVICE_AGAIN).await?;
        store
            .finish_checkout_projection(job.clone(), ProjectionOutcome::Removed)
            .await?;
        return Ok(None);
    }
    let topology = Topology::read(common)?;
    let root = Some(job.checkout.root.as_path());
    let checkout = super::discover(root, super::SERVICE_OUTSIDE, super::SERVICE_AGAIN).await?;
    if checkout.root != job.checkout.root.as_path() || checkout.common != common {
        return Err("The local Git checkout has changed. Update its binding in Settings.".into());
    }
    if Topology::read(common)?.fingerprint != topology.fingerprint {
        return Err("The linked Git worktrees changed while ReviewQ was preparing setup.".into());
    }
    if let Some(key) = files::existing_credential(profile, common)?
        && store
            .retired_grant(Actor::local_operator(), Sha256::digest(key).into())
            .await?
            .value
    {
        clean_owned(profile, common, super::SERVICE_AGAIN).await?;
    }
    let claude = projection::claude_config(claude_profile)?;
    let mut changes = prepare(profile, checkout, url, claude, lock, super::SERVICE_AGAIN).await?;
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
        .finish_checkout_projection(
            job.clone(),
            ProjectionOutcome::Applied {
                verifier: changes.verifier,
                topology: topology.fingerprint,
            },
        )
        .await
    {
        changes.plan.rollback()?;
        return Err(error.into());
    }
    changes.finish()?;
    Ok(Some(topology))
}

/// Gives every checkout ReviewQ wrote into that Settings does not list, so one only
/// `reviewq checkout setup` set up, the address ReviewQ hands out now, such as after a start
/// that has IPv6 loopback where the one before had not. Returns whether one is left to retry.
async fn command_line_setups(
    store: &SqliteStore,
    profile: &Path,
    url: &str,
    listed: &HashSet<AbsolutePath>,
) -> Result<bool, Failure> {
    let mut after = None;
    let mut left = false;
    loop {
        let directories = store.installed_checkout_paths(after).await?;
        let Some(last) = directories.last() else {
            return Ok(left);
        };
        after = Some(last.clone());
        for common in directories
            .iter()
            .filter(|common| !listed.contains(*common))
        {
            let common = common.as_path();
            if let Err(error) = readdress(store, profile, common, url).await {
                left = true;
                eprintln!(
                    "ReviewQ could not give the checkout at {} its new address and tries again: {error}",
                    common.display()
                );
            }
        }
    }
}

/// Runs setup again for the checkout at `common` with its own key, so its grant and bearer
/// stay, when what it wrote names another address than `url`. It uses the Claude config that
/// setup chose. A key whose grant was withdrawn is left for setup or revoke, which remove it.
async fn readdress(
    store: &SqliteStore,
    profile: &Path,
    common: &Path,
    url: &str,
) -> Result<(), Failure> {
    if !files::owned_setup_exists(profile, common)? {
        return Ok(());
    }
    let lock = files::hold(profile, common)?;
    let inventory = projection::Inventory::load(&files::inventory(profile, common)?)?;
    let Some(key) = files::existing_credential(profile, common)? else {
        return Ok(());
    };
    if inventory.addressed(url) {
        return Ok(());
    }
    let verifier = Sha256::digest(key).into();
    if store
        .retired_grant(Actor::local_operator(), verifier)
        .await?
        .value
    {
        return Ok(());
    }
    let (roots, claude) = inventory.places();
    for root in roots {
        // Git finds the checkout from any worktree setup wrote into that is still one of it.
        match git::discover(Some(&root)).await {
            Ok(checkout) if checkout.common == common => {
                let mut changes =
                    prepare(profile, checkout, url, claude, lock, SERVICE_AGAIN).await?;
                changes.apply().await?;
                return changes.finish();
            }
            _ => continue,
        }
    }
    Ok(())
}
