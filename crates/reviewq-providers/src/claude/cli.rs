use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_domain::session::AbsolutePath;
use tokio::process::Command;
use tokio::sync::Mutex;

mod catalog;
mod launch;

pub struct ClaudeCli {
    executable: PathBuf,
    profile: AbsolutePath,
    plugin: Option<AbsolutePath>,
    catalogue: Mutex<Option<(AbsolutePath, Instant, Arc<catalog::Catalogue>)>>,
}

impl ClaudeCli {
    pub fn new(
        executable: PathBuf,
        profile: PathBuf,
        plugin: Option<PathBuf>,
    ) -> Result<Self, AppError> {
        let executable = executable
            .canonicalize()
            .map_err(|_| AppError::Unavailable)?;
        if !executable.is_file() {
            return Err(AppError::Unavailable);
        }
        let profile = directory(profile)?;
        let plugin = plugin.map(directory).transpose()?;
        if plugin.as_ref().is_some_and(|plugin| {
            !plugin
                .as_path()
                .join(".claude-plugin/plugin.json")
                .is_file()
        }) {
            return Err(AppError::InvalidRequest);
        }
        Ok(Self {
            executable,
            profile,
            plugin,
            catalogue: Mutex::new(None),
        })
    }

    fn command(&self, checkout: &Path) -> Command {
        super::jobs::native_command(&self.executable, self.profile.as_path(), checkout)
    }

    fn plugin_args(&self, command: &mut Command) {
        if let Some(plugin) = &self.plugin {
            command.arg("--plugin-dir").arg(plugin.as_path());
        }
    }

    async fn verify_checkout(&self, checkout: &AbsolutePath) -> Result<(), AppError> {
        let path = checkout.as_path();
        if !tokio::fs::canonicalize(path)
            .await
            .is_ok_and(|canonical| canonical == path)
            || !tokio::fs::metadata(path)
                .await
                .is_ok_and(|metadata| metadata.is_dir())
        {
            return Err(AppError::Unavailable);
        }
        Ok(())
    }

    async fn catalogue(&self, checkout: AbsolutePath) -> Result<Arc<catalog::Catalogue>, AppError> {
        self.verify_checkout(&checkout).await?;
        let mut last = self.catalogue.lock().await;
        if let Some((scope, observed, result)) = &*last
            && scope == &checkout
            && observed.elapsed() <= Duration::from_secs(3)
        {
            return Ok(result.clone());
        }
        let observed = Arc::new(catalog::read(self, &checkout).await?);
        *last = Some((checkout, Instant::now(), observed.clone()));
        Ok(observed)
    }
}

fn directory(path: PathBuf) -> Result<AbsolutePath, AppError> {
    let path = path.canonicalize().map_err(|_| AppError::Unavailable)?;
    if !path.is_dir() {
        return Err(AppError::Unavailable);
    }
    AbsolutePath::from_canonical(path).map_err(|_| AppError::Unavailable)
}
