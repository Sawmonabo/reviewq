use std::io::Read;
use std::path::Path;

use reviewq_providers::{WindowsDesktop, WindowsRegistration};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    Failure,
    bundle::{self, Bundle},
};

const ICON: &str = "packaging/notification.png";

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Registration {
    pub(super) owner: String,
    digest: String,
}

impl Registration {
    pub(super) fn plan(bundle: &Bundle, owner: Option<&Self>) -> Result<Option<Self>, Failure> {
        if !WindowsDesktop::available() {
            return if owner.is_some() {
                Err("Windows interop is unavailable for the installed ReviewQ notification identity.".into())
            } else {
                Ok(None)
            };
        }
        let owner = match owner {
            Some(previous) => previous.owner.clone(),
            None => {
                let mut random = [0; 32];
                getrandom::fill(&mut random)
                    .map_err(|_| "Could not reserve the Windows notification identity.")?;
                bundle::hex(&random)
            }
        };
        let digest = bundle
            .manifest
            .files
            .iter()
            .find(|entry| entry.path == ICON)
            .map(|entry| entry.sha256.clone())
            .ok_or("The release is missing the Windows notification icon.")?;
        Ok(Some(Self { owner, digest }))
    }

    pub(super) fn sender(&self) -> Result<WindowsDesktop, Failure> {
        WindowsDesktop::new(self.owner.clone())
            .ok_or("The WSL Windows notification client is unavailable.".into())
    }

    pub(super) async fn preflight(&self, previous: Option<&Self>) -> Result<(), Failure> {
        self.sender()?
            .registration(
                WindowsRegistration::Check,
                &self.digest,
                previous.map(|value| value.digest.as_str()),
                None,
            )
            .await?;
        Ok(())
    }

    pub(super) async fn set(&self, release: &Path, previous: Option<&Self>) -> Result<(), Failure> {
        let mut icon = Vec::new();
        bundle::regular(&release.join(ICON))?
            .take(32 * 1024 + 1)
            .read_to_end(&mut icon)?;
        if icon.len() > 32 * 1024 || bundle::hex(&Sha256::digest(&icon)) != self.digest {
            return Err("The Windows notification icon does not match the release.".into());
        }
        self.sender()?
            .registration(
                WindowsRegistration::Install,
                &self.digest,
                previous.map(|value| value.digest.as_str()),
                Some(&icon),
            )
            .await?;
        Ok(())
    }

    pub(super) async fn remove(&self) -> Result<(), Failure> {
        self.sender()?
            .registration(WindowsRegistration::Remove, &self.digest, None, None)
            .await?;
        Ok(())
    }
}
