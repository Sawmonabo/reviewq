use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use reviewq_domain::identity::ProjectId;

use crate::{native_client, runtime};

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
}

#[derive(Args)]
pub(crate) struct Serve {
    #[arg(long, value_name = "DIR")]
    pub(crate) web: PathBuf,
    #[arg(long, default_value_t = 8766)]
    pub(crate) port: u16,
    #[arg(long, default_value_t = 8767)]
    pub(crate) native_port: u16,
    #[arg(long)]
    pub(crate) gitlab: Option<String>,
    #[arg(long = "project", requires = "gitlab")]
    pub(crate) projects: Vec<ProjectId>,
    /// Path to the configured Codex executable.
    #[arg(long, value_name = "FILE", requires = "codex_home")]
    pub(crate) codex_bin: Option<PathBuf>,
    /// Directory belonging to the configured running Codex daemon.
    #[arg(long, value_name = "DIR", requires = "codex_bin")]
    pub(crate) codex_home: Option<PathBuf>,
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
        Some(Command::Open(open)) => {
            native_client::open(&runtime::profile(options.profile)?, open).await
        }
        None => native_client::open(&runtime::profile(options.profile)?, Open::default()).await,
    }
}
