use std::fs::File;
use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_domain::identity::{GitLabOrigin, ProjectId};
use reviewq_protocol::RevisionDto;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{native_client, runtime};

mod claude;
mod codex;
pub(crate) mod files;
mod git;
mod hooks;
mod observe;
mod projection;
pub(crate) mod reconcile;
mod unfinished;

pub(crate) use unfinished::Unfinished;

type Failure = Box<dyn std::error::Error + Send + Sync>;
const GRANT: &str = "/api/v1/checkout/grant";
/// The line that marks what ReviewQ wrote in a Codex config and in its Git hook.
const MARKER: &str = "# Managed by ReviewQ";
/// What finishes each command's cleanup once the user did what it left for them. The
/// uninstall's step comes from its caller, which also names it in the dashboard job's notice.
const SETUP_AGAIN: &str = "run the same `reviewq checkout setup` command again";
const REVOKE_AGAIN: &str = "run the same `reviewq checkout revoke` command again";
const SERVICE_AGAIN: &str = "ReviewQ tries again on its own";
/// What the user does when the folder a command reads is in no Git checkout.
const SETUP_OUTSIDE: &str = "Run `reviewq checkout setup` inside the Git checkout";
const REVOKE_OUTSIDE: &str = "Run `reviewq checkout revoke` inside the Git checkout";
const SERVICE_OUTSIDE: &str = "Change this project's checkout in ReviewQ's settings";

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
    let checkout = discover(directory.as_deref(), SETUP_OUTSIDE, SETUP_AGAIN).await?;
    let endpoint = runtime::read_endpoint(profile)?;
    let client = native_client::client()?;
    let revision = settings_revision(&client, &endpoint).await?;
    let url = endpoint.mcp_url();
    let claude = projection::claude_config(claude_home.as_deref())?;
    let lock = files::hold(profile, &checkout.common)?;
    // A key whose grant was withdrawn, by a revoke, a settings change or an uninstall whose
    // cleanup did not finish, is never issued again: remove what it set up and make a new one.
    if let Some(key) = files::existing_credential(profile, &checkout.common)? {
        let verifier = URL_SAFE_NO_PAD.encode(Sha256::digest(key));
        let body = serde_json::to_vec(&json!({"action":"retired","verifier":verifier}))?;
        let bytes = native_client::signed(&client, &endpoint, "POST", GRANT, &body).await?;
        if retired(&bytes)? {
            clean_owned(profile, &checkout.common, SETUP_AGAIN).await?;
        }
    }
    let mut changes = prepare(profile, checkout, &url, claude, lock, SETUP_AGAIN).await?;
    let address = json!({
        "settingsRevision":revision, "projectId":project.to_string(),
        "checkout":changes.checkout.root, "commonDir":changes.checkout.common,
        "verifier":URL_SAFE_NO_PAD.encode(changes.verifier),
    });
    let request = serde_json::to_vec(&json!({"action":"issue","address":address}))?;
    let confirm = serde_json::to_vec(&json!({"action":"status","address":address}))?;
    let applied = serde_json::to_vec(&json!({"action":"applied","address":address}))?;
    // A grant issued by an earlier attempt whose reply was lost still counts, so a refused
    // issue is reported only when no grant for this key is in place either.
    let refusal = match native_client::signed(&client, &endpoint, "POST", GRANT, &request).await {
        Ok(bytes) => {
            verify_binding(&bytes, &changes.checkout, project, revision)?;
            None
        }
        Err(error) => Some(error),
    };
    let bytes = match native_client::signed(&client, &endpoint, "POST", GRANT, &confirm).await {
        Ok(bytes) => bytes,
        Err(_) => return Err(refused(refusal, project).into()),
    };
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

/// The checkout Git finds at `directory`, the current folder when `None`, or the error
/// `unreadable` gives.
async fn discover(
    directory: Option<&Path>,
    outside: &str,
    again: &str,
) -> Result<git::Checkout, Failure> {
    git::discover(directory)
        .await
        .map_err(|error| unreadable(directory, outside, again, error))
}

/// What a command says when Git could not read the checkout at `directory`, the current
/// folder when `None`. For a folder in no Git checkout it leads with `outside`, the user's
/// step. Otherwise, such as for a bad line in a worktree's Git config, it gives Git's words
/// and ends with `again`, the step that finishes the command once that is fixed.
fn unreadable(directory: Option<&Path>, outside: &str, again: &str, error: Failure) -> Failure {
    let folder = directory
        .map(Path::to_owned)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let folder = folder
        .canonicalize()
        .or_else(|_| std::path::absolute(&folder))
        .unwrap_or(folder);
    if git::outside_checkout(&folder) {
        let found = format!("ReviewQ found no Git checkout at {}.", folder.display());
        return format!("{outside}. {found}").into();
    }
    let mut left = Unfinished::new(again);
    match error.downcast::<git::Unread>() {
        // Git's words are the cause, said once, at the folder Git could not read.
        Ok(unread) => {
            let cause = Failure::from(unread.cause);
            left.failed("read the Git checkout at", &unread.folder, &cause);
        }
        Err(error) => left.failed("read the Git checkout at", &folder, &error),
    }
    Box::new(left)
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
    /// The step that finishes the command when setup leaves something for the user.
    again: &'static str,
    _lock: File,
}

impl Prepared {
    async fn apply(&mut self) -> Result<(), Failure> {
        self.plan
            .apply(&self.inventory, &self.checkout, self.claude.as_deref())
            .await?;
        match hooks::reconcile(&self.profile, &self.checkout, self.again).await {
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
    again: &'static str,
) -> Result<Prepared, Failure> {
    projection::require_untracked(&checkout, claude.as_deref()).await?;
    let key = files::credential(profile, &checkout.common)?;
    let inventory = files::inventory(profile, &checkout.common)?;
    let previous = projection::Inventory::load(&inventory)?;
    let plan = projection::Plan::new(
        &checkout,
        url,
        &URL_SAFE_NO_PAD.encode(key),
        claude.as_deref(),
        previous,
        &projection::skill(profile, again)?,
        again,
    )
    .await?;
    Ok(Prepared {
        profile: profile.to_owned(),
        checkout,
        claude,
        inventory,
        plan,
        verifier: Sha256::digest(key).into(),
        again,
        _lock: lock,
    })
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
    let common = git::common_directory(directory.as_deref())
        .await
        .map_err(|error| unreadable(directory.as_deref(), REVOKE_OUTSIDE, REVOKE_AGAIN, error))?;
    let endpoint = runtime::read_endpoint(profile)?;
    let client = native_client::client()?;
    let status = native_client::signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
    native_client::verify_ready(&endpoint, &status)?;
    let _lock = files::hold(profile, &common)?;
    let body = serde_json::to_vec(&json!({"action":"revoke","commonDir":common}))?;
    native_client::signed(&client, &endpoint, "POST", GRANT, &body).await?;
    clean_owned(profile, &common, REVOKE_AGAIN)
        .await
        .map_err(|error| {
            format!("ReviewQ revoked management access for this repository. {error}")
        })?;
    println!("ReviewQ management access was revoked for this repository.");
    Ok(())
}

fn retired(bytes: &[u8]) -> Result<bool, Failure> {
    #[derive(Deserialize)]
    struct Envelope {
        data: Retired,
    }
    #[derive(Deserialize)]
    struct Retired {
        retired: bool,
    }
    Ok(serde_json::from_slice::<Envelope>(bytes)?.data.retired)
}

/// Says why ReviewQ refused a grant, from the code the refused issue returned.
fn refused(refusal: Option<Failure>, project: ProjectId) -> String {
    let code = refusal
        .as_ref()
        .and_then(|error| error.downcast_ref::<native_client::Refused>())
        .map(|refused| refused.0.as_str());
    match code {
        Some("stale_revision") => "ReviewQ's settings changed during setup. Run the same `reviewq checkout setup` command again.".into(),
        Some("not_found") => format!("ReviewQ has no configured project {project} whose Git remote is this checkout's origin. Check the project ID, or add the project in ReviewQ's settings."),
        Some("conflict") => format!("ReviewQ's settings bind project {project} to another checkout, or this checkout already has management access for another project. Change the checkout in ReviewQ's settings, or run `reviewq checkout revoke` here first."),
        Some("unavailable") => "ReviewQ could not check this checkout's remote with GitLab. Try again when GitLab is reachable.".into(),
        _ => match refusal {
            Some(cause) => format!("ReviewQ refused management access for this checkout: {cause}."),
            None => "ReviewQ could not confirm management access for this exact checkout and settings revision.".into(),
        },
    }
}

/// Removes what the key of `common` set up, its Git hook included, and forgets the key. What
/// is left for the user comes back as one `Unfinished` that ends with `again`.
async fn clean_owned(profile: &Path, common: &Path, again: &str) -> Result<(), Failure> {
    let hooks = hooks::cleanup(profile, common, again);
    let inventory = files::inventory(profile, common)?;
    let previous = projection::Inventory::load(&inventory)?;
    let key = files::existing_credential(profile, common)?;
    let cleaned = previous
        .cleanup(
            &inventory,
            common,
            &key.map_or_else(String::new, |key| URL_SAFE_NO_PAD.encode(key)),
            again,
        )
        .await;
    if cleaned.is_ok() {
        files::forget(profile, common)?;
    }
    let mut left = Unfinished::new(again);
    for error in [cleaned, hooks].into_iter().filter_map(Result::err) {
        left.absorb(error, common);
    }
    left.result()
}

/// Withdraws every checkout grant and removes what each set up. `again` finishes the command
/// when a cleanup leaves something for the user, such as "run `reviewq uninstall`".
pub(crate) async fn remove_installation(
    profile: &Path,
    store: &reviewq_store::SqliteStore,
    again: &str,
) -> Result<(), Failure> {
    store.retire_installed_grants().await?;
    let mut after = None;
    // Every checkout's steps go in one sentence, so one run of `again` finishes them all.
    let mut left = Unfinished::new(again);
    loop {
        let directories = store.installed_checkout_paths(after).await?;
        let Some(last) = directories.last() else {
            return left.result();
        };
        after = Some(last.clone());
        for common in directories {
            let common = common.as_path();
            let cleanup = async {
                if files::owned_setup_exists(profile, common)? {
                    let _lock = files::hold(profile, common)?;
                    clean_owned(profile, common, again).await?;
                }
                Ok::<_, Failure>(())
            };
            if let Err(error) = cleanup.await {
                left.absorb(error, common);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::Command;

    use super::*;

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "ReviewQ")
            .env("GIT_AUTHOR_EMAIL", "reviewq@example.invalid")
            .env("GIT_COMMITTER_NAME", "ReviewQ")
            .env("GIT_COMMITTER_EMAIL", "reviewq@example.invalid")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {arguments:?}");
    }

    /// A Git config with a bad line makes Git fail just as it does for a folder with no
    /// repository. Cleanup must not take the checkout for someone else's then, report success
    /// and forget the key while the Codex config there still holds its bearer.
    #[tokio::test]
    async fn cleanup_keeps_no_bearer_when_git_cannot_answer() -> Result<(), Failure> {
        let scratch = tempfile::tempdir()?;
        let root = scratch.path().canonicalize()?;
        let (profile, repo) = (root.join("profile"), root.join("repo"));
        std::fs::create_dir(&profile)?;
        git(&root, &["init", "-q", "repo"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
        let url = "http://127.0.0.1:9/mcp";
        // Servers written inline could not hold a table ReviewQ can later prove and remove.
        let config = repo.join(".codex/config.toml");
        std::fs::create_dir(repo.join(".codex"))?;
        let inline = "mcp_servers = { other = { url = \"http://127.0.0.1:1/mcp\" } }\n";
        std::fs::write(&config, inline)?;
        let checkout = git::discover(Some(&repo)).await?;
        let lock = files::hold(&profile, &checkout.common)?;
        let Err(error) = prepare(&profile, checkout, url, None, lock, SETUP_AGAIN).await else {
            panic!("setup refuses servers written inline");
        };
        assert!(
            error.to_string().contains("[mcp_servers.<name>] table"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&config)?, inline);
        std::fs::remove_file(&config)?;
        let checkout = git::discover(Some(&repo)).await?;
        let common = checkout.common.clone();
        let lock = files::hold(&profile, &common)?;
        let mut changes = prepare(&profile, checkout, url, None, lock, SETUP_AGAIN).await?;
        changes
            .plan
            .apply(&changes.inventory, &changes.checkout, None)
            .await?;
        changes.finish()?;
        let key = files::existing_credential(&profile, &common)?.ok_or("no key")?;
        let bearer = URL_SAFE_NO_PAD.encode(key);
        let written = std::fs::read_to_string(&config)?;
        assert!(written.contains(&bearer));
        // The user's comment above ReviewQ's table, which ends the file, stays as it was.
        let user = "model = \"o3\"\n\n# Kept by the user\n";
        let marker = "# Managed by ReviewQ";
        // A copy of ReviewQ's marker line goes with it.
        let edited = written.replacen(
            &format!("\n{marker}"),
            &format!("{user}{marker}\n{marker}"),
            1,
        );
        std::fs::write(&config, edited)?;

        std::fs::OpenOptions::new()
            .append(true)
            .open(common.join("config"))?
            .write_all(b"[broken\n")?;
        let error = clean_owned(&profile, &common, "run `reviewq uninstall`")
            .await
            .expect_err("cleanup stops while Git cannot answer");
        assert!(error.to_string().contains("Git could not say"), "{error}");
        assert_eq!(std::fs::read_to_string(&config)?, user);
        assert!(files::existing_credential(&profile, &common)?.is_some());
        Ok(())
    }

    /// A linked worktree whose Git folder is gone, as after the main repository moved, is no
    /// checkout. Setup must skip it, and cleanup must still remove the table ReviewQ wrote
    /// there, by its own proof, and finish, rather than wait for Git for good. A comment the
    /// user wrote above that table stays there when its URL changes, and stays before the
    /// header that follows it once the table is removed.
    #[tokio::test]
    async fn a_worktree_whose_git_folder_is_gone_blocks_nothing() -> Result<(), Failure> {
        let scratch = tempfile::tempdir()?;
        let root = scratch.path().canonicalize()?;
        let (profile, repo) = (root.join("profile"), root.join("repo"));
        std::fs::create_dir(&profile)?;
        git(&root, &["init", "-q", "repo"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
        git(&repo, &["worktree", "add", "-q", "../linked"]);
        git(&repo, &["worktree", "add", "-q", "../edited"]);
        git(&repo, &["worktree", "add", "-q", "../tracked"]);
        git(&repo, &["worktree", "add", "-q", "../untracked"]);
        let config = root.join("linked/.codex/config.toml");
        let skill = |worktree: &str| {
            root.join(worktree)
                .join(".agents/skills/reviewq-manage/SKILL.md")
        };
        let (user, marker) = ("# Kept by the user\n", "# Managed by ReviewQ");
        let other = "\n[mcp_servers.other]\nurl = \"http://127.0.0.1:1/mcp\"\n";
        for url in ["http://127.0.0.1:9/mcp", "http://[::1]:10/mcp"] {
            let checkout = git::discover(Some(&repo)).await?;
            let lock = files::hold(&profile, &checkout.common)?;
            let mut changes = prepare(&profile, checkout, url, None, lock, SETUP_AGAIN).await?;
            changes
                .plan
                .apply(&changes.inventory, &changes.checkout, None)
                .await?;
            changes.finish()?;
            let written = std::fs::read_to_string(&config)?;
            assert!(written.contains(url));
            if !written.contains(user) {
                let edited = written.replacen(marker, &format!("{user}{marker}"), 1);
                std::fs::write(&config, format!("{edited}{other}"))?;
            }
        }
        let text = std::fs::read_to_string(&config)?;
        assert!(text.contains(&format!("\n{user}{marker}\n")) && text.ends_with(other));
        // The skill ReviewQ wrote goes with the folder; one changed by hand is the user's.
        std::fs::write(skill("edited"), "# Changed by the user\n")?;
        // Where another repository now stands, a copy of the skill it tracks is its own, even
        // with the same bytes; an untracked copy there is ReviewQ's.
        let shared = std::fs::read(skill("linked"))?;
        for name in ["tracked", "untracked"] {
            std::fs::remove_dir_all(root.join(name))?;
            git(&root, &["init", "-q", name]);
            std::fs::create_dir_all(skill(name).parent().ok_or("no folder")?)?;
            std::fs::write(skill(name), &shared)?;
        }
        git(&root.join("tracked"), &["add", "."]);
        git(&root.join("tracked"), &["commit", "-q", "-m", "skill"]);

        let common = git::discover(Some(&repo)).await?.common;
        let moved = root.join("moved");
        std::fs::rename(&repo, &moved)?;
        // A `.git` link to nothing, which Git passes over, holds no repository either.
        std::fs::remove_file(root.join("edited/.git"))?;
        std::os::unix::fs::symlink(root.join("gone"), root.join("edited/.git"))?;
        assert_eq!(git::discover(Some(&moved)).await?.worktrees, [moved]);
        clean_owned(&profile, &common, SETUP_AGAIN).await?;
        assert_eq!(
            std::fs::read_to_string(&config)?,
            format!("\n{user}{other}")
        );
        assert!(!skill("linked").exists());
        assert!(!root.join("edited/.codex/config.toml").exists());
        // Its own folder goes once empty; the one holding the hand-edited skill stays.
        assert!(!root.join("linked/.agents/skills/reviewq-manage").exists());
        assert!(root.join("linked/.agents/skills").is_dir());
        assert_eq!(
            std::fs::read_to_string(skill("edited"))?,
            "# Changed by the user\n"
        );
        assert_eq!(std::fs::read(skill("tracked"))?, shared);
        assert!(!skill("untracked").exists());
        assert!(files::existing_credential(&profile, &common)?.is_none());
        Ok(())
    }

    /// The MCP address changes when a start has the IPv6 listener and the next has not, or
    /// the other way round. Setup must take its own entries at the old address for ReviewQ's
    /// and rewrite them, in every worktree and in Claude's config, and cleanup must remove them.
    #[tokio::test]
    async fn a_changed_mcp_address_is_rewritten_and_cleaned_up() -> Result<(), Failure> {
        let scratch = tempfile::tempdir()?;
        let root = scratch.path().canonicalize()?;
        let (profile, repo, claude) =
            (root.join("profile"), root.join("repo"), root.join("claude"));
        std::fs::create_dir(&profile)?;
        std::fs::create_dir(&claude)?;
        let claude = claude.join(".claude.json");
        git(&root, &["init", "-q", "repo"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
        git(&repo, &["worktree", "add", "-q", "../linked"]);
        let configs = [
            repo.join(".codex/config.toml"),
            root.join("linked/.codex/config.toml"),
        ];
        // Each file keeps its own line ends, and whether it ends with one, throughout.
        let originals = ["model = \"o3\"\r\n# Kept\r\n", "model = \"o3\""];
        for (file, original) in configs.iter().zip(originals) {
            std::fs::create_dir(file.parent().ok_or("no folder")?)?;
            std::fs::write(file, original)?;
        }
        let (ipv4, ipv6) = ("http://127.0.0.1:9/mcp", "http://[::1]:10/mcp");
        for (url, other) in [(ipv4, ipv6), (ipv6, ipv4), (ipv4, ipv6)] {
            let checkout = git::discover(Some(&repo)).await?;
            let lock = files::hold(&profile, &checkout.common)?;
            let selected = Some(claude.clone());
            let mut changes = prepare(&profile, checkout, url, selected, lock, SETUP_AGAIN).await?;
            changes
                .plan
                .apply(
                    &changes.inventory,
                    &changes.checkout,
                    changes.claude.as_deref(),
                )
                .await?;
            changes.finish()?;
            for file in configs.iter().chain([&claude]) {
                let text = std::fs::read_to_string(file)?;
                assert!(
                    text.contains(url) && !text.contains(other),
                    "{}",
                    file.display()
                );
            }
        }
        let crlf = std::fs::read_to_string(&configs[0])?;
        assert_eq!(crlf.matches('\n').count(), crlf.matches("\r\n").count());
        assert!(!std::fs::read_to_string(&configs[1])?.ends_with('\n'));
        // Once the Claude entry is changed by hand, setup that no longer selects that config
        // leaves it alone and finishes, rather than stopping every checkout's setup on it.
        let edited = std::fs::read_to_string(&claude)?.replace(ipv4, "http://127.0.0.1:1/mcp");
        std::fs::write(&claude, &edited)?;
        let checkout = git::discover(Some(&repo)).await?;
        let lock = files::hold(&profile, &checkout.common)?;
        let mut changes = prepare(&profile, checkout, ipv4, None, lock, SETUP_AGAIN).await?;
        changes
            .plan
            .apply(&changes.inventory, &changes.checkout, None)
            .await?;
        changes.finish()?;
        assert_eq!(std::fs::read_to_string(&claude)?, edited);

        let common = git::discover(Some(&repo)).await?.common;
        clean_owned(&profile, &common, SETUP_AGAIN).await?;
        for (file, original) in configs.iter().zip(originals) {
            assert_eq!(std::fs::read_to_string(file)?, original);
        }
        assert_eq!(std::fs::read_to_string(&claude)?, edited);
        Ok(())
    }
}
