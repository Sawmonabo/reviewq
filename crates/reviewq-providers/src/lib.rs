//! External source and native integrations.

mod checkout;
mod claude;
mod codex;
mod desktop;
mod gitlab;
mod history;
mod http;
mod jira;
mod question_wire;
mod settings;
mod terminal;
mod workflow;

pub use checkout::git_output;
pub use claude::{Claude, ClaudeCli, ClaudeWorkflow};
pub use codex::{
    Codex, HookRevocation, approve_hooks as approve_codex_hooks, revoke_hooks as revoke_codex_hooks,
};
pub use desktop::{
    LinuxDesktopNotifier, WindowsDesktop, WindowsDesktopNotifier, WindowsRegistration,
    open_windows_browser,
};
pub use gitlab::GitLab;
pub use history::FileHistory;
pub use http::SourceNetwork;
pub use settings::SourceAccounts;
pub use terminal::{Terminal, TerminalProvider};
pub use workflow::Workflows;
