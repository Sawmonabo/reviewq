use std::path::Path;
use std::time::Duration;

use tokio::sync::watch;

use super::{Failure, Release, files, load, paths::Paths, probe};

pub(crate) struct Managed {
    paths: Paths,
    release: Release,
    expected_store: Option<String>,
}

impl Managed {
    pub(crate) fn windows_sender(
        &self,
    ) -> Result<Option<reviewq_providers::WindowsDesktop>, Failure> {
        self.release.windows_sender()
    }

    pub(crate) async fn verify(profile: &Path, digest: &str) -> Result<Self, Failure> {
        let paths = Paths::resolve()?;
        if paths.profile != profile {
            return Err("The managed service profile does not match the installation.".into());
        }
        let inventory = load(&paths)?;
        if inventory.removing {
            return Err("Finish uninstalling ReviewQ before starting the user service.".into());
        }
        let pending = inventory.pending.as_ref();
        let release = pending
            .map(|operation| &operation.next)
            .into_iter()
            .chain(inventory.active.as_ref())
            .find(|record| record.binary == digest && files::verify_current(&paths, record).is_ok())
            .cloned()
            .ok_or("The managed executable does not match the active or pending release.")?;
        let expected_store = pending
            .and_then(|operation| operation.store_id.clone())
            .or(inventory.store_id);
        if (pending.is_none() || expected_store.is_some())
            && probe(&paths, release.store).await? != expected_store
        {
            return Err("The retained store does not match the installed ReviewQ profile.".into());
        }
        Ok(Self {
            paths,
            release,
            expected_store,
        })
    }

    pub(crate) fn check_store(&self, id: &str, format: i64) -> Result<(), Failure> {
        if self.release.store != format
            || self.expected_store.as_deref().is_some_and(|old| old != id)
        {
            return Err("The managed daemon opened a different store.".into());
        }
        Ok(())
    }

    pub(crate) async fn wait(
        &self,
        id: &str,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<bool, Failure> {
        loop {
            if *shutdown.borrow_and_update() {
                return Ok(false);
            }
            let state = load(&self.paths)?;
            if state.removing {
                return Ok(false);
            }
            if state.pending.is_none() {
                if state.active.as_ref() != Some(&self.release)
                    || state.store_id.as_deref() != Some(id)
                {
                    return Err("The accepted activation does not match this service.".into());
                }
                return Ok(true);
            }
            tokio::select! {
                _ = shutdown.changed() => {},
                () = tokio::time::sleep(Duration::from_millis(100)) => {},
            }
        }
    }
}
