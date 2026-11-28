//! The management tools ReviewQ's MCP server lists for ordinary Claude and Codex chats: their
//! names, descriptions and input schemas.

use serde::{Deserialize, Serialize};

use crate::{
    GitShaDto, HistoryEntryIdDto, HostProviderDto, MessageTextDto, ResourceIdDto, RevisionDto,
    RunIdDto, SessionKeyDto, SubmissionIdDto,
};

/// What the model reads when a Claude session calls a ReviewQ tool by a name ReviewQ does not
/// serve, as one still running an older plugin or tool list does after an update.
pub const UNKNOWN_TOOL_CLAUDE: &str = "This ReviewQ tool is not available; ask the user to run /reload-plugins to load the current ReviewQ tools.";

/// What the model reads when a Codex session calls a ReviewQ tool by a name ReviewQ does not
/// serve, as one still holding an older tool list does after an update.
pub const UNKNOWN_TOOL_CODEX: &str = "This ReviewQ tool is not available; ask the user to restart this Codex session to load the current ReviewQ tools.";

/// One management tool.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ManagementTool {
    SearchReviews,
    GetReview,
    ReadAgentTranscript,
    SendAgentMessage,
    StartReview,
    AnswerAgentQuestion,
    SetThreadFollowups,
}

/// Every management tool, in the order the MCP server lists them.
pub const MANAGEMENT_TOOLS: [ManagementTool; 7] = [
    ManagementTool::SearchReviews,
    ManagementTool::GetReview,
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
                "Lists the reviews of this checkout's configured projects, each with its summary and dashboard link, and when ReviewQ last synced them. filter picks all, unstarted, working, waiting, needs_input, agent_questions or archived, and defaults to needs_input; offset and limit page through the list."
            }
            Self::GetReview => {
                "Reads one review, named in target by its ticket, merge request or registration, with its reviewing agents' runs, its follow-ups and dashboard links. For the lead agent, or the session sessionKey names, it also returns the published report (its summary, or with report a section of it), the agent's pending questions, its posting action, sessions and recovery, and the delivery of the last message or of the one messageSubmissionId names. launchSubmissionId reads what became of a start_review call."
            }
            Self::ReadAgentTranscript => {
                "Reads a bounded page of a reviewing agent's transcript, the lead's or the one sessionKey names. read.kind page reads entries from a cursor, direction or aroundId; content reads one long entry by the ref a page returned. Follow the returned continuation for more."
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
            Self::SearchReviews | Self::GetReview | Self::ReadAgentTranscript => false,
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
