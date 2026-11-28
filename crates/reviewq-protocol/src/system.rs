//! The System page: facts about the running ReviewQ and its start, quit and uninstall controls.

use serde::{Deserialize, Serialize};

use crate::{SafeCountDto, Success, UtcTimestampDto};

/// What the System page shows about the running ReviewQ.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SystemFacts {
    pub version: String,
    pub mode: SystemMode,
    /// The dashboard address, such as `http://127.0.0.1:8766`.
    pub address: String,
    /// The data folder for display, with the home folder shortened to `~`.
    pub data_folder: String,
    /// The size of the data "Also delete my data" removes; null when it cannot be read.
    pub data_bytes: Option<SafeCountDto>,
    /// The queue's last successful refresh.
    pub last_refresh_at: Option<UtcTimestampDto>,
    /// Whether the service starts at login; null unless `mode` is `installed`.
    pub start_at_login: Option<bool>,
}

/// How this ReviewQ was started. Only `installed` offers the login switch, Quit and Uninstall.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SystemMode {
    Installed,
    Foreground,
    Validation,
}

/// Turns starting at login on or off. It never starts or stops ReviewQ now.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LoginStartRequest {
    pub enabled: bool,
}

/// The login state read back from systemd after the change.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LoginStart {
    pub start_at_login: bool,
}

/// Uninstalls ReviewQ, and with `deleteData` also deletes its data once everything else is
/// removed.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UninstallRequest {
    pub delete_data: bool,
}

pub type SystemFactsResponse = Success<SystemFacts>;
pub type LoginStartResponse = Success<LoginStart>;
