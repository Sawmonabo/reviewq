//! External source and native integrations.

mod claude;
mod codex;
mod desktop;
mod gitlab;
mod history;
mod http;
mod report_file;
mod terminal;
mod workflow;

pub use claude::Claude;
pub use codex::Codex;
pub use desktop::LinuxDesktopNotifier;
pub use gitlab::GitLab;
pub use history::FileHistory;
pub use http::SourceNetwork;
pub use report_file::RegisteredReportFiles;
pub use terminal::{Terminal, TerminalProvider};
pub use workflow::Workflows;
