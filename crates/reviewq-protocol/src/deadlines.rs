//! How long ReviewQ takes at most to answer one call of a tool a model calls, on either host,
//! and what the model reads when it does not answer in time.

use std::time::Duration;

use crate::ledger::LedgerTool;

/// The least time a host gives one call of a tool on ReviewQ's MCP server before it ends the
/// call with its own timeout, which tells the model nothing about what ReviewQ did. Claude Code
/// ends each request to an HTTP MCP server after 60 seconds unless `MCP_TOOL_TIMEOUT` or the
/// server's `timeout` raises it. Codex 0.172.0 waits for the server's `tool_timeout_sec`, 300
/// seconds when it is unset. ReviewQ's MCP entries set neither.
const HOST_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// How much longer than a tool's deadline the Claude plugin waits for the bridge process, which
/// spends the deadline on the service and then prints ReviewQ's own sentence. No host limit
/// binds a call of the plugin's own tools: Claude Code's hook budget does not count "a
/// `tool.call` or `tool.run` hook on a tool its own plugin registered", so the plugin's wait on
/// `$.process.run` is the only limit. The margin covers the process's start, its endpoint read
/// and its reply on a loaded machine, where a stalled disk can hold any of them for seconds.
const PLUGIN_MARGIN: Duration = Duration::from_secs(30);

/// A call that reads, or files one record.
const SHORT: Duration = Duration::from_secs(30);
/// A call that publishes a report version, reads GitLab or starts native work.
const LONG: Duration = Duration::from_secs(55);

/// How long the MCP server waits for the store to check a request's grant before it answers
/// without running the request. The check counts toward the call's own deadline, so it gets the
/// shortest one.
pub const GRANT_CHECK_LIMIT: Duration = SHORT;

// ReviewQ's answer, not the host's timeout, ends every call, with time left to carry it back.
const _: () = assert!(LONG.as_secs() + 5 <= HOST_TOOL_TIMEOUT.as_secs());

/// What the model reads when a ledger call passes its deadline. The service may still have
/// stored the call, so a blind repeat of a revision-guarded call would be refused as stale.
const LEDGER_TIMED_OUT: &str = "ReviewQ did not answer this ledger call in time, but it may have stored it. Read the ledger with review_ledger before repeating the call.";
/// What the model reads when a management call that changes something passes its deadline.
/// Each such call carries a `submissionId`, and a repeat with the same one returns the stored
/// outcome instead of acting twice.
const CHANGE_TIMED_OUT: &str = "ReviewQ did not answer this call in time, but it may have stored it. Read the review with GetReview before repeating the call, and repeat it only with the same submissionId.";
/// What the model reads when `review_action` passes its deadline.
const ACTION_TIMED_OUT: &str = "ReviewQ did not answer this action in time, but it may have recorded it. Call review_action with operation reconcile before declaring the action again.";
/// What the model reads when a read passes its deadline.
const READ_TIMED_OUT: &str =
    "ReviewQ did not answer this call in time. The call changes nothing, so it can be repeated.";

/// How long ReviewQ takes at most to answer one call of a tool, and the sentence the model
/// reads in its place when that time passes.
#[derive(Clone, Copy, Debug)]
pub struct ToolDeadline {
    pub limit: Duration,
    pub timed_out: &'static str,
}

impl ToolDeadline {
    /// How long the Claude plugin waits for the bridge's answer to a call of this tool.
    pub fn plugin_wait(&self) -> Duration {
        self.limit + PLUGIN_MARGIN
    }
}

/// The deadline of the tool a host lists as `name`: a ledger tool, a management tool of the MCP
/// server, or the Claude plugin's `review_action`. It holds for the whole call on either host:
/// the Claude bridge's status check and request together, and the MCP server's handling of a
/// call from its arrival, the grant check and admission included. `finish_review` publishes a
/// version, so it gets the longer limit, and so does every call that reads GitLab or starts
/// native work. A name ReviewQ does not serve gets a read's deadline; its call is refused at
/// once.
pub fn tool_deadline(name: &str) -> ToolDeadline {
    let (limit, timed_out) = match (LedgerTool::from_name(name), name) {
        (Some(LedgerTool::FinishReview), _) => (LONG, LEDGER_TIMED_OUT),
        (Some(_), _) => (SHORT, LEDGER_TIMED_OUT),
        (
            None,
            "SendReviewMessage" | "StartReview" | "AnswerReviewQuestion" | "ConfigureFollowups",
        ) => (LONG, CHANGE_TIMED_OUT),
        (None, "review_action") => (LONG, ACTION_TIMED_OUT),
        (None, _) => (SHORT, READ_TIMED_OUT),
    };
    ToolDeadline { limit, timed_out }
}
