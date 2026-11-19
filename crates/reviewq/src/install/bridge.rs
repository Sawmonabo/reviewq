use std::fs;
use std::path::Path;

use reviewq_transport::{BridgeDescriptor, ServiceEndpoint};

use super::{
    Failure,
    paths::{self, Paths},
};

const LOCATOR: &str = "bridge.json";

pub(super) fn check(paths: &Paths, owned_installation: bool) -> Result<(), Failure> {
    let path = paths.config.join(LOCATOR);
    if let Some(saved) = paths::read_json::<BridgeDescriptor>(&path)?
        && (!owned_installation
            || saved.installation_id != crate::runtime::profile_id(&paths.profile))
    {
        return Err(paths::conflict(&path));
    }
    Ok(())
}

pub(super) fn remove(paths: &Paths) -> Result<(), Failure> {
    check(paths, true)?;
    let path = paths.config.join(LOCATOR);
    if paths::exists(&path)? {
        fs::remove_file(path)?;
        paths::sync(&paths.config)?;
    }
    Ok(())
}

pub(crate) fn publish(endpoint: &ServiceEndpoint) -> Result<(), Failure> {
    let paths = Paths::resolve()?;
    check(&paths, true)?;
    paths::write_json(&paths.config.join(LOCATOR), &endpoint.bridge_descriptor())
}

pub(crate) fn clear(endpoint: &ServiceEndpoint) -> Result<(), Failure> {
    let paths = Paths::resolve()?;
    let path = paths.config.join(LOCATOR);
    if let Some(saved) = paths::read_json::<BridgeDescriptor>(&path)? {
        if saved != endpoint.bridge_descriptor() {
            return Err(paths::conflict(&path));
        }
        fs::remove_file(path)?;
        paths::sync(&paths.config)?;
    }
    Ok(())
}

pub(crate) fn verify(profile: &Path, endpoint: &ServiceEndpoint) -> Result<(), Failure> {
    let paths = Paths::resolve()?;
    if paths.profile != profile || super::installed(&paths)?.is_none() {
        return Ok(());
    }
    let path = paths.config.join(LOCATOR);
    if paths::read_json::<BridgeDescriptor>(&path)?.as_ref() != Some(&endpoint.bridge_descriptor())
    {
        return Err(format!(
            "The native bridge does not match the active service at {}.",
            path.display()
        )
        .into());
    }
    Ok(())
}
