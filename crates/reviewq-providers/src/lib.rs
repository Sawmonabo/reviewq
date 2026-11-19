//! External source and native integrations.

mod checkout;
mod claude;
mod codex;
mod desktop;
mod gitlab;
mod history;
mod http;
mod jira;
mod report_file;
mod settings;
mod terminal;
mod workflow;

pub use checkout::git_output;
pub use claude::{Claude, ClaudeWorkflow};
pub use codex::Codex;
pub use desktop::LinuxDesktopNotifier;
pub use gitlab::GitLab;
pub use history::FileHistory;
pub use http::SourceNetwork;
pub use report_file::RegisteredReportFiles;
pub use settings::SourceAccounts;
pub use terminal::{Terminal, TerminalProvider};
pub use workflow::Workflows;
