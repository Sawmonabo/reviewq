//! Argument-vector launch and exact shell-quoted commands for native conversations.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{NativeTerminal, TerminalHandoff, TerminalInstruction};
use reviewq_domain::session::HostProvider;
use tokio::process::Command;

use crate::claude::jobs::{Job, JobKind, claude_job, native_command};

pub struct Terminal {
    codex: Option<TerminalProvider>,
    claude: Option<TerminalProvider>,
    wsl_distribution: Option<String>,
}

pub struct TerminalProvider {
    pub executable: PathBuf,
    pub profile: PathBuf,
    pub profile_is_explicit: bool,
}

impl Terminal {
    pub fn new(
        codex: Option<TerminalProvider>,
        claude: Option<TerminalProvider>,
        wsl_distribution: Option<String>,
    ) -> Self {
        Self {
            codex,
            claude,
            wsl_distribution,
        }
    }
}

impl NativeTerminal for Terminal {
    fn terminal(&self, session: HistorySession, open: bool) -> AppFuture<'_, TerminalHandoff> {
        Box::pin(async move {
            use TerminalHandoff::Instruction;
            if session.parent.is_some() {
                return Ok(Instruction(TerminalInstruction::InspectParent));
            }
            let cwd = session.binding.cwd().as_path();
            if !tokio::fs::canonicalize(cwd)
                .await
                .is_ok_and(|path| path == cwd && path.is_dir())
            {
                return Ok(Instruction(TerminalInstruction::RestoreCheckout));
            }
            let native = session.binding.identity();
            let id = native.native_id().as_str();
            if !uuid::Uuid::parse_str(id)
                .is_ok_and(|value| !value.is_nil() && value.hyphenated().to_string() == id)
            {
                return Err(AppError::Conflict);
            }
            let (provider, root_variable) = match native.provider() {
                HostProvider::Codex => (self.codex.as_ref(), "CODEX_HOME"),
                HostProvider::Claude => (self.claude.as_ref(), "CLAUDE_CONFIG_DIR"),
            };
            let Some(provider) = provider else {
                return Ok(Instruction(TerminalInstruction::ConfigureProvider));
            };
            let profile = tokio::fs::canonicalize(&provider.profile)
                .await
                .map_err(|_| AppError::Unavailable)?;
            if profile != native.profile().as_path() {
                return Ok(Instruction(TerminalInstruction::ConfigureProvider));
            }
            let Ok(executable) = tokio::fs::canonicalize(&provider.executable).await else {
                return Ok(Instruction(TerminalInstruction::ConfigureProvider));
            };
            if !executable.is_file() {
                return Ok(Instruction(TerminalInstruction::ConfigureProvider));
            }
            let config_dir = provider.profile_is_explicit.then_some(profile.as_path());
            let (operation, argument) = match native.provider() {
                HostProvider::Codex => ("resume", id.to_owned()),
                HostProvider::Claude => {
                    match claude_job(native_command(&executable, config_dir, cwd), cwd, id).await? {
                        Some(Job {
                            kind: JobKind::Interactive,
                            pid: Some(_),
                            ..
                        }) => return Ok(Instruction(TerminalInstruction::ForegroundClaude)),
                        Some(Job {
                            kind: JobKind::Background,
                            id: Some(job),
                            pid: Some(_),
                            ..
                        }) => ("attach", job),
                        _ => ("--resume", id.to_owned()),
                    }
                }
            };
            let cwd = utf8(cwd)?;
            let assignment;
            let mut argv = if let Some(config_dir) = config_dir {
                assignment = format!("{root_variable}={}", utf8(config_dir)?);
                vec!["env", &assignment]
            } else {
                vec!["env", "-u", root_variable]
            };
            argv.extend([utf8(&executable)?, operation, &argument]);
            let command = format!(
                "cd {} && {}",
                shlex::try_quote(cwd).map_err(|_| AppError::Unavailable)?,
                shlex::try_join(argv.iter().copied()).map_err(|_| AppError::Unavailable)?
            );
            let opened = if open && let Some(distro) = &self.wsl_distribution {
                open_wsl(distro, cwd, &argv).await
            } else {
                false
            };
            Ok(TerminalHandoff::Command { command, opened })
        })
    }
}

async fn open_wsl(distro: &str, cwd: &str, argv: &[&str]) -> bool {
    if distro.starts_with('-')
        || distro.is_empty()
        || [distro, cwd]
            .into_iter()
            .chain(argv.iter().copied())
            .any(|value| value.contains(';') || value.chars().any(char::is_control))
    {
        return false;
    }
    let mut command = Command::new("wt.exe");
    command
        .args([
            "new-tab",
            "--title",
            "ReviewQ",
            "wsl.exe",
            "--distribution",
            distro,
            "--cd",
            cwd,
            "--exec",
        ])
        .args(argv)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    tokio::time::timeout(Duration::from_secs(3), command.status())
        .await
        .is_ok_and(|status| status.is_ok_and(|status| status.success()))
}

fn utf8(path: &Path) -> Result<&str, AppError> {
    path.to_str().ok_or(AppError::Unavailable)
}
