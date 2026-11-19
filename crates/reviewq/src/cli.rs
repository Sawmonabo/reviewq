use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use reviewq_domain::identity::{GitSha, NotificationId, ProjectId, SubmissionId};

use crate::{checkout_setup, native_client, runtime};

mod action;
mod report;
mod workflow;
mod workspace;

type Failure = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser)]
#[command(
    version,
    about = "Work with registered reviews and their native conversations"
)]
struct CommandLine {
    #[arg(long, global = true)]
    profile: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the dashboard and API in the foreground.
    Serve(Serve),
    /// Open a private browser session.
    Open(Open),
    /// Check the identity and readiness of the running service.
    Status,
    /// Refresh the queue from configured sources.
    Refresh,
    /// Link the current Codex terminal to an exact review without opening a browser.
    Link { target: String },
    /// Record a local decision at the exact listed merge-request head.
    Review {
        target: String,
        sha: GitSha,
        #[arg(value_enum)]
        disposition: workspace::Disposition,
        /// Reuse this ID to recover an earlier decision response.
        #[arg(long)]
        submission_id: Option<SubmissionId>,
    },
    /// Mark only the named notification receipts as read.
    Ack {
        #[arg(required = true, num_args = 1..=2000)]
        ids: Vec<NotificationId>,
    },
    /// Deliver a report from the current registered Codex tool call.
    Report {
        #[command(flatten)]
        scope: workflow::Profile,
        #[command(subcommand)]
        command: report::Report,
    },
    /// Record or reconcile publication intent for the current registered lead.
    Action {
        #[command(flatten)]
        scope: workflow::Profile,
        #[command(subcommand)]
        command: action::Action,
    },
    /// Manage the authorization of a local Git checkout.
    #[command(subcommand)]
    Checkout(Checkout),
    #[command(subcommand, hide = true)]
    Internal(Internal),
}

#[derive(Subcommand)]
enum Internal {
    SetupWorktree {
        #[arg(long)]
        settings: PathBuf,
        #[arg(long)]
        common_dir: PathBuf,
    },
    ClaudeHook,
    ClaudeBridge {
        #[arg(value_enum)]
        operation: native_client::ClaudeBridgeOperation,
        session_id: String,
    },
    ClaudeCompanion {
        session_id: String,
        #[arg(long, allow_hyphen_values = true)]
        target: Option<String>,
    },
}

#[derive(Subcommand)]
enum Checkout {
    /// Configure private ReviewQ management in this project.
    Setup {
        project: ProjectId,
        #[arg(long, value_name = "DIR")]
        directory: Option<PathBuf>,
        #[arg(long, value_name = "DIR")]
        claude_home: Option<PathBuf>,
    },
    /// Revoke the management credential for this repository.
    Revoke {
        #[arg(long, value_name = "DIR")]
        directory: Option<PathBuf>,
    },
}

#[derive(Args)]
pub(crate) struct Serve {
    #[arg(long, value_name = "DIR")]
    pub(crate) web: Option<PathBuf>,
    #[arg(long)]
    pub(crate) port: Option<u16>,
    #[arg(long)]
    pub(crate) native_port: Option<u16>,
    #[arg(long, requires = "validation")]
    pub(crate) gitlab: Option<String>,
    #[arg(long = "project", requires = "gitlab")]
    pub(crate) projects: Vec<ProjectId>,
    /// Path to the configured Codex executable.
    #[arg(long, value_name = "FILE", requires = "codex_home")]
    pub(crate) codex_bin: Option<PathBuf>,
    /// Directory belonging to the configured running Codex daemon.
    #[arg(long, value_name = "DIR", requires = "codex_bin")]
    pub(crate) codex_home: Option<PathBuf>,
    /// Exact Codex model to send when starting an idle linked turn.
    #[arg(long, requires_all = ["codex_bin", "codex_effort"])]
    pub(crate) codex_model: Option<String>,
    /// Exact Codex reasoning effort paired with the configured model.
    #[arg(long, requires = "codex_model")]
    pub(crate) codex_effort: Option<String>,
    /// Directory belonging to the selected Claude Code profile.
    #[arg(long, value_name = "DIR")]
    pub(crate) claude_home: Option<PathBuf>,
    /// Path to the configured Claude Code executable.
    #[arg(long, value_name = "FILE")]
    pub(crate) claude_bin: Option<PathBuf>,
    /// WSL distribution used when opening a terminal.
    #[arg(long)]
    pub(crate) wsl_distribution: Option<String>,
    #[arg(long, requires = "runtime_root")]
    pub(crate) validation: bool,
    #[arg(long, requires = "validation")]
    pub(crate) runtime_root: Option<PathBuf>,
}

#[derive(Default, Args)]
pub(crate) struct Open {
    /// Print the transient launch URL for an explicit local handoff.
    #[arg(long)]
    pub(crate) print_url: bool,
}

pub(crate) fn run() -> Result<(), Failure> {
    let options = CommandLine::parse();
    let mut runtime = if matches!(options.command, Some(Command::Serve(_))) {
        let mut runtime = tokio::runtime::Builder::new_multi_thread();
        runtime.worker_threads(2);
        runtime
    } else {
        tokio::runtime::Builder::new_current_thread()
    };
    runtime.enable_all().build()?.block_on(execute(options))
}

async fn execute(options: CommandLine) -> Result<(), Failure> {
    match options.command {
        Some(Command::Serve(serve)) => {
            let profile = match (options.profile, serve.runtime_root.as_ref()) {
                (Some(_), Some(_)) => return Err("use either --profile or --runtime-root".into()),
                (None, Some(root)) => root.clone(),
                (profile, None) => runtime::profile(profile)?,
            };
            runtime::serve(profile, serve).await
        }
        Some(Command::Status) => native_client::status(&runtime::profile(options.profile)?).await,
        Some(Command::Refresh) => native_client::refresh(&runtime::profile(options.profile)?).await,
        Some(Command::Link { target }) => {
            native_client::link(&runtime::profile(options.profile)?, target).await
        }
        Some(Command::Review {
            target,
            sha,
            disposition,
            submission_id,
        }) => {
            workspace::review(
                &runtime::profile(options.profile)?,
                target,
                sha,
                disposition,
                submission_id,
            )
            .await
        }
        Some(Command::Ack { ids }) => {
            workspace::ack(&runtime::profile(options.profile)?, ids).await
        }
        Some(Command::Report { scope, command }) => {
            report::execute(options.profile, scope, command).await
        }
        Some(Command::Action { scope, command }) => {
            action::execute(options.profile, scope, command).await
        }
        Some(Command::Checkout(Checkout::Setup {
            project,
            directory,
            claude_home,
        })) => {
            checkout_setup::install(
                &runtime::profile(options.profile)?,
                directory,
                project,
                claude_home,
            )
            .await
        }
        Some(Command::Checkout(Checkout::Revoke { directory })) => {
            checkout_setup::revoke(&runtime::profile(options.profile)?, directory).await
        }
        Some(Command::Internal(Internal::ClaudeHook)) => {
            if let Ok(profile) = runtime::profile(options.profile) {
                let _ = native_client::claude_hook(&profile).await;
            }
            Ok(())
        }
        Some(Command::Internal(Internal::SetupWorktree {
            settings,
            common_dir,
        })) => {
            checkout_setup::worktree(&runtime::profile(options.profile)?, settings, common_dir)
                .await
        }
        Some(Command::Internal(Internal::ClaudeBridge {
            operation,
            session_id,
        })) => {
            native_client::claude_bridge(&runtime::profile(options.profile)?, operation, session_id)
                .await
        }
        Some(Command::Internal(Internal::ClaudeCompanion { session_id, target })) => {
            native_client::claude(&runtime::profile(options.profile)?, session_id, target).await
        }
        Some(Command::Open(open)) => {
            native_client::open(&runtime::profile(options.profile)?, open).await
        }
        None => native_client::open(&runtime::profile(options.profile)?, Open::default()).await,
    }
}
