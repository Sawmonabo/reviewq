use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::process::Command;

use super::{
    Failure, Release,
    bundle::Bundle,
    paths::{self, Paths},
};

const NAME: &str = "reviewq@reviewq";

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Registration {
    pub executable: PathBuf,
    pub home: PathBuf,
    pub version: String,
}

#[derive(Deserialize)]
struct Market {
    name: String,
    source: String,
    path: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Plugin {
    id: String,
    scope: String,
    version: String,
    enabled: bool,
    install_path: PathBuf,
    read_from_folder: Option<PathBuf>,
    folder_version: Option<String>,
    #[serde(default)]
    errors: Vec<String>,
}

struct Observed {
    market: Option<Market>,
    plugin: Option<Plugin>,
}

pub(super) fn plan(
    paths: &Paths,
    bundle: &Bundle,
    executable: PathBuf,
) -> Result<Registration, Failure> {
    #[derive(Deserialize)]
    struct Manifest {
        name: String,
        version: String,
    }
    let path = bundle
        .root
        .join("hosts/claude/plugin/.claude-plugin/plugin.json");
    let mut text = Vec::new();
    super::bundle::regular(&path)?
        .take(16 * 1024 + 1)
        .read_to_end(&mut text)?;
    if text.len() > 16 * 1024 {
        return Err("Claude plugin manifest exceeds its limit.".into());
    }
    let value: Manifest = serde_json::from_slice(&text)?;
    if value.name != "reviewq"
        || value.version.len() > 128
        || value.version.is_empty()
        || !value
            .version
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b".+-".contains(&ch))
    {
        return Err("Invalid ReviewQ plugin identity.".into());
    }
    Ok(Registration {
        executable,
        home: paths.claude_home.clone(),
        version: value.version,
    })
}

pub(super) async fn validate(
    paths: &Paths,
    bundle: &Bundle,
    next: &Registration,
) -> Result<(), Failure> {
    let config = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&paths.state)?;
    let isolated = Registration {
        home: config.path().to_owned(),
        ..next.clone()
    };
    let market = bundle.root.join("hosts/claude");
    for directory in [&market, &market.join("plugin")] {
        isolated
            .call(
                paths,
                &[
                    "plugin",
                    "validate",
                    "--strict",
                    "--json",
                    string(directory)?,
                ],
                false,
            )
            .await?;
    }
    Ok(())
}

pub(super) async fn preflight(
    paths: &Paths,
    old: Option<&Release>,
    next: &Release,
    pending: bool,
) -> Result<(), Failure> {
    let before = registration(old);
    let after = next.claude.as_ref();
    if let Some(old) = before
        && after.is_none_or(|next| next.home != old.home)
    {
        return Err(
            "Restore the installed Claude Code profile and executable before updating ReviewQ."
                .into(),
        );
    }
    if let Some(host) = after.or(before) {
        let seen = host.observe(paths).await?;
        ownership(paths, &seen, old, pending.then_some(next))?;
    }
    Ok(())
}

pub(super) async fn activate(
    paths: &Paths,
    old: Option<&Release>,
    next: &Release,
) -> Result<(), Failure> {
    let Some(host) = next.claude.as_ref() else {
        return Ok(());
    };
    let mut seen = host.observe(paths).await?;
    ownership(paths, &seen, old, Some(next))?;
    if seen.loaded(paths, next) {
        return Ok(());
    }
    let stable = marketplace(paths);
    if seen.market.as_ref().is_some_and(|market| {
        market.path.as_ref() != Some(&stable)
            && market.path.as_ref() != Some(&release_marketplace(paths, next))
    }) {
        host.remove_owned(paths, &seen).await?;
        seen = host.observe(paths).await?;
    }
    if seen.market.is_some() && seen.plugin.is_some() {
        let _ = host
            .call(paths, &["plugin", "marketplace", "update", "reviewq"], true)
            .await?;
        let _ = host
            .call(paths, &["plugin", "update", NAME, "--scope", "user"], true)
            .await?;
        seen = host.observe(paths).await?;
        ownership(paths, &seen, old, Some(next))?;
        if seen.loaded(paths, next) {
            return Ok(());
        }
        if seen.plugin.is_some() {
            host.uninstall(paths).await?;
        }
    }
    host.call(
        paths,
        &[
            "plugin",
            "install",
            "reviewq",
            "--marketplace",
            string(&stable)?,
            "--scope",
            "user",
        ],
        false,
    )
    .await?;
    let observed = host.observe(paths).await?;
    ownership(paths, &observed, None, Some(next))?;
    if !observed.loaded(paths, next) {
        return Err(
            "Claude Code could not confirm the installed ReviewQ source and version.".into(),
        );
    }
    Ok(())
}

pub(super) async fn remove(paths: &Paths, previous: &Release) -> Result<(), Failure> {
    let Some(host) = previous.claude.as_ref() else {
        return Ok(());
    };
    let seen = host.observe(paths).await?;
    ownership(paths, &seen, Some(previous), None)?;
    host.remove_owned(paths, &seen).await
}

impl Registration {
    async fn call(
        &self,
        paths: &Paths,
        args: &[&str],
        allow_failure: bool,
    ) -> Result<(bool, String), Failure> {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .env("CLAUDE_CONFIG_DIR", &self.home)
            .current_dir(&paths.state);
        crate::service::output(command, allow_failure).await
    }

    async fn json<T: DeserializeOwned>(&self, paths: &Paths, args: &[&str]) -> Result<T, Failure> {
        let (_, output) = self.call(paths, args, false).await?;
        Ok(serde_json::from_str(&output)?)
    }

    async fn observe(&self, paths: &Paths) -> Result<Observed, Failure> {
        let markets: Vec<Market> = self
            .json(paths, &["plugin", "marketplace", "list", "--json"])
            .await?;
        let mut selected = markets.into_iter().filter(|item| item.name == "reviewq");
        let market = selected.next();
        let plugins: Vec<Plugin> = self.json(paths, &["plugin", "list", "--json"]).await?;
        let mut installed = plugins.into_iter().filter(|item| item.id == NAME);
        let plugin = installed.next();
        if selected.next().is_some() || installed.next().is_some() {
            return Err("Preserved ambiguous Claude Code ReviewQ registrations.".into());
        }
        Ok(Observed { market, plugin })
    }

    async fn uninstall(&self, paths: &Paths) -> Result<(), Failure> {
        self.call(
            paths,
            &[
                "plugin",
                "uninstall",
                NAME,
                "--scope",
                "user",
                "--keep-data",
            ],
            false,
        )
        .await?;
        Ok(())
    }

    async fn remove_owned(&self, paths: &Paths, seen: &Observed) -> Result<(), Failure> {
        if seen.plugin.is_some() {
            self.uninstall(paths).await?;
        }
        if seen.market.is_some() {
            self.call(
                paths,
                &[
                    "plugin",
                    "marketplace",
                    "remove",
                    "reviewq",
                    "--scope",
                    "user",
                ],
                false,
            )
            .await?;
        }
        let remaining = self.observe(paths).await?;
        if remaining.plugin.is_some() || remaining.market.is_some() {
            return Err("ReviewQ registrations remain in Claude Code.".into());
        }
        Ok(())
    }
}

impl Observed {
    fn loaded(&self, paths: &Paths, release: &Release) -> bool {
        let Some(wanted) = release.claude.as_ref() else {
            return false;
        };
        self.market
            .as_ref()
            .is_some_and(|market| matches_market(market, paths, release))
            && self.plugin.as_ref().is_some_and(|plugin| {
                plugin.enabled
                    && plugin.errors.is_empty()
                    && plugin.version == wanted.version
                    && plugin
                        .folder_version
                        .as_deref()
                        .is_none_or(|version| version == wanted.version)
            })
    }
}

fn ownership(
    paths: &Paths,
    actual: &Observed,
    old: Option<&Release>,
    next: Option<&Release>,
) -> Result<(), Failure> {
    let candidates = [old, next];
    if let Some(market) = actual.market.as_ref()
        && !candidates
            .into_iter()
            .flatten()
            .any(|release| matches_market(market, paths, release))
    {
        return Err(paths::conflict(
            market.path.as_deref().unwrap_or(&paths.claude_home),
        ));
    }
    if let Some(plugin) = actual.plugin.as_ref()
        && (actual.market.is_none()
            || plugin.scope != "user"
            || !candidates.into_iter().flatten().any(|release| {
                release.claude.as_ref().is_some_and(|host| {
                    plugin.version == host.version
                        && plugin.install_path.starts_with(host.home.join("plugins"))
                        && plugin.read_from_folder.as_ref().is_none_or(|path| {
                            *path == marketplace(paths).join("plugin")
                                || candidates.into_iter().flatten().any(|release| {
                                    *path == release_marketplace(paths, release).join("plugin")
                                })
                        })
                })
            }))
    {
        return Err(paths::conflict(&plugin.install_path));
    }
    Ok(())
}

fn matches_market(market: &Market, paths: &Paths, release: &Release) -> bool {
    release.claude.is_some()
        && market.source == "directory"
        && market.path.as_ref().is_some_and(|path| {
            *path == marketplace(paths) || *path == release_marketplace(paths, release)
        })
}

fn registration(release: Option<&Release>) -> Option<&Registration> {
    release.and_then(|item| item.claude.as_ref())
}

fn marketplace(paths: &Paths) -> PathBuf {
    paths.profile.join("current/hosts/claude")
}

fn release_marketplace(paths: &Paths, release: &Release) -> PathBuf {
    paths
        .profile
        .join("releases")
        .join(&release.id)
        .join("hosts/claude")
}

fn string(path: &Path) -> Result<&str, Failure> {
    path.to_str()
        .ok_or_else(|| "Claude Code plugin paths must be UTF-8.".into())
}
