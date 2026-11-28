//! The management tools ReviewQ's MCP server lists for ordinary Claude and Codex chats: their
//! names, descriptions and input schemas.

use serde::{Deserialize, Serialize};

use crate::{
    GitShaDto, HistoryEntryIdDto, HostProviderDto, MessageTextDto, ResourceIdDto, RevisionDto,
    RunIdDto, SessionKeyDto, SubmissionIdDto,
};

/// Writes the name of the MCP server entry that carries the management tools to a Claude
/// session, as a literal, so the sentence below can quote it at compile time.
macro_rules! claude_management_server {
    () => {
        "reviewq-manage"
    };
}

/// The name of the MCP server entry ReviewQ writes into a checkout's Claude configuration. It
/// differs from the plugin's name because Claude Code names a plugin's tools
/// `mcp__<plugin>__…`, so a server called `reviewq` shares the plugin's name and Claude refuses
/// the plugin's tools beside it.
pub const CLAUDE_MANAGEMENT_SERVER: &str = claude_management_server!();

/// What the model reads when a Claude session calls one of the plugin's ReviewQ tools by a
/// name ReviewQ does not serve, as a session still running an older plugin does after an
/// update.
pub const UNKNOWN_TOOL_CLAUDE: &str = "This ReviewQ tool is not available; ask the user to run /reload-plugins to load the current ReviewQ tools.";

/// What the model reads when a Claude session calls ReviewQ's MCP server by a tool name it
/// does not serve. The management tools reach Claude through the checkout's MCP server entry,
/// not the plugin, so `/reload-plugins` leaves them as they were; Claude Code reads a server's
/// tools again when the user reconnects it from `/mcp`.
pub const UNKNOWN_MANAGEMENT_TOOL_CLAUDE: &str = concat!(
    "This ReviewQ tool is not available; ask the user to run /mcp, select the ",
    claude_management_server!(),
    " server and choose Reconnect to load the current ReviewQ tools."
);

/// What the model reads when a Codex session calls a ReviewQ tool by a name ReviewQ does not
/// serve, as one still holding an older tool list does after an update.
pub const UNKNOWN_TOOL_CODEX: &str = "This ReviewQ tool is not available; ask the user to restart this Codex session to load the current ReviewQ tools.";

/// One management tool.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ManagementTool {
    SearchReviews,
    GetReview,
    GetFindings,
    ReadAgentTranscript,
    SendAgentMessage,
    StartReview,
    AnswerAgentQuestion,
    SetThreadFollowups,
}

/// Every management tool, in the order the MCP server lists them.
pub const MANAGEMENT_TOOLS: [ManagementTool; 8] = [
    ManagementTool::SearchReviews,
    ManagementTool::GetReview,
    ManagementTool::GetFindings,
    ManagementTool::ReadAgentTranscript,
    ManagementTool::SendAgentMessage,
    ManagementTool::StartReview,
    ManagementTool::AnswerAgentQuestion,
    ManagementTool::SetThreadFollowups,
];

impl ManagementTool {
    /// The tool name the MCP server lists.
    pub const fn name(self) -> &'static str {
        match self {
            Self::SearchReviews => "search_reviews",
            Self::GetReview => "get_review",
            Self::GetFindings => "get_findings",
            Self::ReadAgentTranscript => "read_agent_transcript",
            Self::SendAgentMessage => "send_agent_message",
            Self::StartReview => "start_review",
            Self::AnswerAgentQuestion => "answer_agent_question",
            Self::SetThreadFollowups => "set_thread_followups",
        }
    }

    /// The description the model reads.
    pub const fn description(self) -> &'static str {
        match self {
            Self::SearchReviews => {
                "Finds reviews of this checkout's configured projects by text and status, a page at a time. query matches what the dashboard's search matches (merge request title, project path, author, ticket key and title) and the titles of the findings any published report version of the review showed; a review found by a finding title names that finding as matchedFinding, with its run's sessionKey and the receiptId of the newest version that showed it, ready for get_findings. filter picks all, unstarted, working, waiting, needs_input, agent_questions or archived, and both apply; it defaults to all with a query and to needs_input without one. offset and limit page through the list. Returns each review's summary and dashboard link, the total, nextOffset and when ReviewQ last synced the projects."
            }
            Self::GetReview => {
                "Opens one review, named in target by its ticket, merge request or registration: its reviewing agents' runs, paged with offset and limit, its follow-ups and dashboard links. For the lead agent's run, or the run of the session sessionKey names, it also returns the agent's pending questions, its posting action, sessions and recovery, the delivery of the last message or of the one messageSubmissionId names, and one read of the run's published report chosen by report: summary, the default, gives a version's summary; versions lists the run's published versions newest first, each with its receiptId, publication time, verdict, reviewed commit and parent version, paged by cursor and limit; section gives a version's text from offset. summary and section read the current version unless receiptId names an older one. launchSubmissionId reads what became of a start_review call."
            }
            Self::GetFindings => {
                "Reads the findings of one published report version of a review named in target: the lead agent's run, or the run of the session sessionKey names, at its current version or the older one receiptId names. state picks open findings, the ones the version shows, retired findings, the ones the version closed with an outcome such as fixed, or all, the default; kind picks functional or quality findings. Returns the version and its findings in ID order, each with its title, severity, blocking, kind, state, outcome, locations, issue, consequence, evidence and proposed comment. Follow nextCursor with the same arguments for the rest. Only published versions are read; a run with none has no findings to return."
            }
            Self::ReadAgentTranscript => {
                "Reads a reviewing agent's transcript a bounded page at a time, the lead's or the one sessionKey names. read.kind page reads entries from a cursor, direction or aroundId; search finds the entries whose text contains query, oldest first or, with direction older, newest first, and returns each match's entryId with the text around it, which a page read with that entry as aroundId shows in place; content reads one long entry by the ref a page returned. Follow the returned continuation for more."
            }
            Self::SendAgentMessage => {
                "Queues a message for the review's lead agent, or the one sessionKey names, only when the user asked you to send it. Give the user's text, and a new submissionId, which you reuse only to retry the same message; finding points the message at a finding or passage of a published report version. Returns the delivery receipt: queued is not yet delivered."
            }
            Self::StartReview => {
                "Starts a review of target with the provider the user chose, only when the user asked for it. Give a new submissionId, which you reuse only to retry the same start; invocation, context, model and effort choose how the agent starts. Returns the new run and its dashboard link, or the saved attempt while it is still starting."
            }
            Self::AnswerAgentQuestion => {
                "Answers a question that a reviewing agent asked through its host's own question prompt: give that prompt's requestId and one answer per question. Only a Codex agent's prompt can be answered here; a Claude agent's question is answered in its terminal or in ReviewQ. A review paused with wait_for_user is not answered with this tool: send the agent a message with send_agent_message, or type the reply in its terminal."
            }
            Self::SetThreadFollowups => {
                "Sets which GitLab discussions of a review ReviewQ follows up on for its lead agent, or pauses, resumes or runs those follow-ups now, only when the user asked for it. configure takes the threads to follow and their state; give a new submissionId and the expectedGeneration you last read. A follow-up check reassesses the threads privately and posts nothing."
            }
        }
    }

    /// Whether a call changes something, so a repeat must carry the same `submissionId`.
    pub const fn changes(self) -> bool {
        match self {
            Self::SearchReviews
            | Self::GetReview
            | Self::GetFindings
            | Self::ReadAgentTranscript => false,
            Self::SendAgentMessage
            | Self::StartReview
            | Self::AnswerAgentQuestion
            | Self::SetThreadFollowups => true,
        }
    }

    /// The tool the MCP server lists as `name`, if it is a management tool.
    pub fn from_name(name: &str) -> Option<Self> {
        MANAGEMENT_TOOLS
            .into_iter()
            .find(|tool| tool.name() == name)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SearchReviews {
    /// Text to look for; empty or absent searches nothing and lists by filter alone.
    #[cfg_attr(feature = "schema", schemars(length(max = 200)))]
    pub query: Option<String>,
    pub filter: Option<ReviewFilter>,
    pub offset: Option<u32>,
    pub limit: Option<u16>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReviewFilter {
    All,
    Unstarted,
    Working,
    Waiting,
    NeedsInput,
    AgentQuestions,
    Archived,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GetReview {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub offset: Option<u32>,
    pub limit: Option<u16>,
    pub launch_submission_id: Option<SubmissionIdDto>,
    pub message_submission_id: Option<SubmissionIdDto>,
    pub session_cursor: Option<crate::PageCursorDto>,
    pub session_limit: Option<u16>,
    pub question_id: Option<ResourceIdDto>,
    pub report: Option<ReportRead>,
}

#[derive(Deserialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind",
    deny_unknown_fields
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReportRead {
    Summary {
        receipt_id: Option<ResourceIdDto>,
    },
    Section {
        receipt_id: Option<ResourceIdDto>,
        offset: Option<crate::SafeCountDto>,
        characters: Option<u32>,
    },
    Versions {
        cursor: Option<String>,
        limit: Option<u16>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GetFindings {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub receipt_id: Option<ResourceIdDto>,
    pub state: Option<FindingState>,
    pub kind: Option<crate::reports::FindingKind>,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

/// Which findings of a published version a read returns. A version shows its open findings
/// and records the outcome that retired each finding it closed; it holds nothing about a
/// finding retired by an earlier version.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FindingState {
    Open,
    Retired,
    All,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReadAgentTranscript {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub read: TranscriptRead,
}

#[derive(Deserialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind",
    deny_unknown_fields
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TranscriptRead {
    Page {
        cursor: Option<String>,
        limit: Option<u16>,
        direction: Option<crate::history::Direction>,
        around_id: Option<HistoryEntryIdDto>,
        locator: Option<String>,
    },
    Content {
        #[serde(rename = "ref")]
        reference: String,
        cursor: Option<String>,
    },
    Search {
        query: String,
        cursor: Option<String>,
        direction: Option<crate::history::Direction>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SendAgentMessage {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub submission_id: SubmissionIdDto,
    pub text: MessageTextDto,
    pub finding: Option<crate::reports::FeedbackReference>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StartReview {
    pub target: String,
    pub provider: HostProviderDto,
    pub submission_id: SubmissionIdDto,
    pub expected_sha: Option<GitShaDto>,
    pub invocation: Option<crate::launch::Invocation>,
    pub context: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SetThreadFollowups {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub run_id: Option<RunIdDto>,
    pub action: FollowupAction,
    pub submission_id: SubmissionIdDto,
    pub expected_generation: Option<RevisionDto>,
    pub threads: Option<Vec<crate::followups::Selection>>,
    pub state: Option<crate::followups::EnrollmentState>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FollowupAction {
    Configure,
    Pause,
    Resume,
    ReviewNow,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AnswerAgentQuestion {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub request_id: ResourceIdDto,
    pub submission_id: SubmissionIdDto,
    pub answers: std::collections::BTreeMap<String, crate::questions::Values>,
}
