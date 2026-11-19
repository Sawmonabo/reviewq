use std::process::Stdio;
use std::time::Duration;

use reviewq_app::access::AppError;
use reviewq_domain::session::AbsolutePath;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::ClaudeCli;

const MAX_CONTROL_BYTES: u64 = 1024 * 1024;
const MAX_CATALOGUE_ENTRIES: usize = 512;

#[derive(Deserialize)]
pub(super) struct Catalogue {
    pub(super) commands: Vec<NativeCommand>,
    pub(super) models: Vec<Model>,
}

#[derive(Deserialize)]
pub(super) struct NativeCommand {
    pub(super) name: String,
    #[serde(default)]
    pub(super) builtin: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Model {
    pub(super) value: String,
    pub(super) display_name: String,
    pub(super) supported_effort_levels: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    response: Option<Control>,
}

#[derive(Deserialize)]
struct Control {
    subtype: String,
    request_id: String,
    response: Option<Catalogue>,
}

pub(super) async fn read(cli: &ClaudeCli, checkout: &AbsolutePath) -> Result<Catalogue, AppError> {
    let mut command = cli.command(checkout.as_path());
    command
        .args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--setting-sources",
            "user,project,local",
        ])
        .stdin(Stdio::piped());
    cli.plugin_args(&mut command);
    let mut child = command.spawn().map_err(|_| AppError::Unavailable)?;
    let request_id = format!("reviewq-{}", uuid::Uuid::new_v4());
    let request = serde_json::json!({
        "type": "control_request", "request_id": request_id,
        "request": {"subtype": "initialize", "hooks": null},
    });
    let mut stdin = child.stdin.take().ok_or(AppError::Unavailable)?;
    let stdout = child.stdout.take().ok_or(AppError::Unavailable)?;
    let mut reader = BufReader::new(stdout.take(MAX_CONTROL_BYTES + 1));
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        stdin
            .write_all(request.to_string().as_bytes())
            .await
            .map_err(|_| AppError::Unavailable)?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|_| AppError::Unavailable)?;
        stdin.flush().await.map_err(|_| AppError::Unavailable)?;
        read_response(&mut reader, &request_id).await
    })
    .await
    .map_err(|_| AppError::Unavailable)?;
    // The catalogue connection has no user turn. Closing its private stdin lets it exit.
    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_millis(300), child.wait()).await;
    result
}

async fn read_response(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    request: &str,
) -> Result<Catalogue, AppError> {
    let mut consumed = 0_u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        let length = reader
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| AppError::Unavailable)?;
        consumed += length as u64;
        if consumed > MAX_CONTROL_BYTES {
            return Err(AppError::PayloadTooLarge);
        }
        if length == 0 || !line.ends_with(b"\n") {
            return Err(AppError::Unavailable);
        }
        let frame: Envelope = serde_json::from_slice(&line).map_err(|_| AppError::Unavailable)?;
        if frame.kind != "control_response" {
            continue;
        }
        let control = frame.response.ok_or(AppError::Unavailable)?;
        if control.request_id != request {
            continue;
        }
        if control.subtype != "success" {
            return Err(AppError::Unavailable);
        }
        let result = control.response.ok_or(AppError::Unavailable)?;
        if result.commands.len() > MAX_CATALOGUE_ENTRIES
            || result.models.len() > MAX_CATALOGUE_ENTRIES
        {
            return Err(AppError::PayloadTooLarge);
        }
        return Ok(result);
    }
}

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use reviewq_app::access::AppFuture;
use reviewq_app::launch::{NativeModel, NativeModelReader};
use reviewq_app::settings::NativeSkillReader;
use reviewq_domain::settings::NativeSkill;

impl NativeModelReader for ClaudeCli {
    fn models(&self, checkout: AbsolutePath) -> AppFuture<'_, Vec<NativeModel>> {
        Box::pin(async move {
            let catalogue = self.catalogue(checkout).await?;
            let mut models = Vec::new();
            let mut ids = BTreeSet::new();
            for model in &catalogue.models {
                if !valid_native_label(&model.value, 256)
                    || model.value.starts_with('-')
                    || !valid_native_label(&model.display_name, 256)
                    || !ids.insert(&model.value)
                {
                    return Err(AppError::Unavailable);
                }
                let efforts = model
                    .supported_effort_levels
                    .iter()
                    .flatten()
                    .filter(|value| {
                        matches!(value.as_str(), "low" | "medium" | "high" | "xhigh" | "max")
                    })
                    .cloned()
                    .collect();
                models.push(NativeModel {
                    id: model.value.clone(),
                    label: model.display_name.clone(),
                    is_default: model.value == "default",
                    default_effort: None,
                    efforts,
                });
            }
            if models.is_empty() {
                return Err(AppError::Unavailable);
            }
            Ok(models)
        })
    }
}

impl NativeSkillReader for ClaudeCli {
    fn skills(&self, checkout: AbsolutePath) -> AppFuture<'_, Vec<NativeSkill>> {
        Box::pin(async move {
            let catalogue = self.catalogue(checkout.clone()).await?;
            let mut names = BTreeSet::new();
            let mut output = Vec::new();
            for item in &catalogue.commands {
                let name = item.name.trim_start_matches('/');
                if item.builtin || !valid_command(name) || !names.insert(name) {
                    continue;
                }
                if let Some(path) = skill_path(self, &checkout, name).await? {
                    output.push(NativeSkill {
                        name: name.to_owned(),
                        path,
                    });
                }
            }
            Ok(output)
        })
    }
}

async fn skill_path(
    cli: &ClaudeCli,
    checkout: &AbsolutePath,
    name: &str,
) -> Result<Option<AbsolutePath>, AppError> {
    let mut candidates = Vec::new();
    let checkout_root = checkout.as_path().join(".claude");
    if let Some((plugin, skill)) = name.split_once(':')
        && plugin == "reviewq"
        && !skill.contains(':')
        && let Some(path) = &cli.plugin
    {
        candidates.push(path.as_path().join("skills").join(skill).join("SKILL.md"));
    }
    for root in [&checkout_root, cli.profile.as_path()] {
        native_conventional_paths(root, name, &mut candidates);
    }
    let mut matching = BTreeSet::new();
    for path in candidates {
        if let Ok(path) = tokio::fs::canonicalize(path).await
            && tokio::fs::metadata(&path)
                .await
                .is_ok_and(|metadata| metadata.is_file())
        {
            matching.insert(path);
        }
    }
    if matching.len() != 1 {
        return Ok(None);
    }
    matching
        .into_iter()
        .next()
        .map(AbsolutePath::from_canonical)
        .transpose()
        .map_err(|_| AppError::Unavailable)
}

fn native_conventional_paths(root: &Path, name: &str, output: &mut Vec<PathBuf>) {
    if !name.contains(':') {
        output.push(root.join("skills").join(name).join("SKILL.md"));
    }
    let command = name
        .split(':')
        .fold(root.join("commands"), |path, part| path.join(part));
    output.push(command.with_extension("md"));
}

fn valid_command(value: &str) -> bool {
    value.len() <= 256
        && value.split(':').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

fn valid_native_label(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}
