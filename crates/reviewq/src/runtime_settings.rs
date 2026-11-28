use std::fs::OpenOptions;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

const MAX_BYTES: u64 = 8192;
/// The start door listens this far above the browser port.
const START_PORT_OFFSET: u16 = 4;
type Failure = Box<dyn std::error::Error + Send + Sync>;

#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Settings {
    pub port: u16,
    pub poll_seconds: u64,
    pub desktop_notifications: bool,
    pub claude_command: Option<String>,
    pub codex_command: Option<String>,
    pub chromium_command: Option<String>,
    pub wsl_distribution: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: 8766,
            poll_seconds: 300,
            desktop_notifications: true,
            claude_command: None,
            codex_command: None,
            chromium_command: None,
            wsl_distribution: None,
        }
    }
}

pub(crate) fn config_path(profile: &Path, validation: bool) -> PathBuf {
    if validation || crate::runtime::profile(None).as_deref().ok() != Some(profile) {
        return profile.join("runtime.json");
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".config"))
        })
        .map(|root| root.join("reviewq/runtime.json"))
        .unwrap_or_else(|| profile.join("runtime.json"))
}

impl Settings {
    /// Loads one private, non-secret startup snapshot before binding or opening SQLite.
    pub(crate) fn load(profile: &Path, validation: bool) -> Result<Self, Failure> {
        let path = config_path(profile, validation);
        let invalid = |detail: &str| -> Failure {
            format!("Invalid runtime settings at {} ({detail})", path.display()).into()
        };
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Self {
                    port: if validation { 8768 } else { 8766 },
                    ..Self::default()
                });
            }
            Err(_) => return Err(invalid("cannot open private file")),
        };
        let owner = rustix::process::getuid().as_raw();
        let metadata = file
            .metadata()
            .map_err(|_| invalid("cannot inspect file"))?;
        if !metadata.is_file()
            || metadata.uid() != owner
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(invalid("expected an owner-only regular file"));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("cannot read file"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("file exceeds 8 KiB"));
        }
        let fields: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&bytes).map_err(|_| invalid("expected a JSON object"))?;
        for (key, value) in &fields {
            let valid = match key.as_str() {
                "port" => value
                    .as_u64()
                    .is_some_and(|value| (1024..=65531).contains(&value)),
                "pollSeconds" => value.as_u64().is_some_and(|value| {
                    value >= 60
                        && tokio::time::Instant::now()
                            .checked_add(Duration::from_secs(value))
                            .is_some()
                }),
                "desktopNotifications" => value.is_boolean(),
                "wslDistribution" => value.as_str().is_some_and(|value| {
                    value.len() <= 256
                        && !value.starts_with('-')
                        && !value.chars().any(char::is_control)
                }),
                "claudeCommand" | "codexCommand" | "chromiumCommand" => {
                    value.as_str().is_some_and(valid_command)
                }
                _ => return Err(invalid("unexpected property")),
            };
            if !valid {
                return Err(invalid(key));
            }
        }
        let mut settings: Self = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("duplicate or unsupported property"))?;
        if validation && !fields.contains_key("port") {
            settings.port = 8768;
        }
        Ok(settings)
    }

    pub(crate) fn ports(
        &self,
        browser: Option<u16>,
        native: Option<u16>,
    ) -> Result<(u16, u16), Failure> {
        let browser = browser.unwrap_or(self.port);
        let native = match native {
            Some(port) => port,
            None if browser == 0 => 0,
            None => browser
                .checked_add(1)
                .ok_or("browser port leaves no port for the native listener")?,
        };
        if (browser != 0 && browser < 1024)
            || (native != 0 && native < 1024)
            || (browser != 0 && browser == native)
            || (browser != 0 && start_port(browser) == Some(native))
        {
            return Err("browser, native and start ports must be separate and unprivileged".into());
        }
        Ok((browser, native))
    }
}

/// The installed service's start door: the loopback port a Start click connects to while
/// ReviewQ is stopped. It is always the browser port plus four, so the stopped page works it
/// out from its own address. `None` when the browser port leaves no room for it.
pub(crate) fn start_port(browser: u16) -> Option<u16> {
    browser.checked_add(START_PORT_OFFSET)
}

fn valid_command(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && (path.is_absolute() || !value.contains('/'))
}

pub(crate) fn executable(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    let valid = |path: &Path| {
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    if path.is_absolute() {
        return valid(path).then(|| path.to_path_buf());
    }
    let search = std::env::var_os("PATH")?;
    std::env::split_paths(&search)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(path))
        .find(|path| valid(path))
}
