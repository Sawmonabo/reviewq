//! External source and native integrations.

mod claude;
mod codex;
mod gitlab;
mod history;
mod http;

pub use claude::Claude;
pub use codex::Codex;
pub use gitlab::GitLab;
pub use history::FileHistory;
