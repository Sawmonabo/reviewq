//! The findings-ledger tool contract: the six tools, their inputs and results, the native route
//! bodies, and the one validator every entry point calls.

use std::sync::LazyLock;

use serde_json::{Map, Value};

mod inputs;
mod results;
mod routes;
mod validate;

pub use inputs::{
    AssessFinding, FinishReview, LedgerInput, MAX_ASSESSMENTS, MAX_CATEGORY_CHARS,
    MAX_DETAILS_BYTES, MAX_DISCUSSION_IDS, MAX_EXTRA_TITLE_CHARS, MAX_LOCATIONS,
    MAX_NOTE_BODY_BYTES, MAX_READ_IDS, MAX_TITLE_CHARS, MAX_TOOL_INPUT_BYTES, NeedsInput,
    NoteSection, RecordFinding, RecordNote, ReviewLedger,
};
pub use results::{
    FindingLine, FindingReceipt, FindingRecord, FinishReceipt, HeldReceipt, LedgerPage, LedgerPass,
    LedgerRecords, LedgerSummary, NoteLine, NoteReceipt, NoteRecord, PassKind, RecordedOutcome,
};
pub use routes::{
    GateReply, GateRequest, LedgerBinding, LedgerRequest, ReadbackReply, ReadbackRequest,
};
#[cfg(feature = "schema")]
pub use validate::validate_against;
pub use validate::validate_ledger_input;

/// The generated input schemas, one self-contained Draft 7 schema per tool name.
const SCHEMAS: &str = include_str!("../../../contracts/ledger-tools.schema.json");

static TOOL_SCHEMAS: LazyLock<Map<String, Value>> = LazyLock::new(|| {
    serde_json::from_str(SCHEMAS).expect("the generated ledger tool schemas are a JSON object")
});

/// One of the six ledger tools.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LedgerTool {
    RecordFinding,
    AssessFinding,
    RecordNote,
    ReviewLedger,
    FinishReview,
    NeedsInput,
}

/// Who may call a tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LedgerCaller {
    /// The registered lead or one of its subagents.
    LeadOrSubagent,
    /// Only the registered lead itself.
    Lead,
}

/// Every ledger tool, in the order hosts list them.
pub const LEDGER_TOOLS: [LedgerTool; 6] = [
    LedgerTool::RecordFinding,
    LedgerTool::AssessFinding,
    LedgerTool::RecordNote,
    LedgerTool::ReviewLedger,
    LedgerTool::FinishReview,
    LedgerTool::NeedsInput,
];

impl LedgerTool {
    /// The tool name both hosts register.
    pub const fn name(self) -> &'static str {
        match self {
            Self::RecordFinding => "record_finding",
            Self::AssessFinding => "assess_finding",
            Self::RecordNote => "record_note",
            Self::ReviewLedger => "review_ledger",
            Self::FinishReview => "finish_review",
            Self::NeedsInput => "needs_input",
        }
    }

    /// The tool's route segment under `/api/v1/native/ledger/`.
    pub const fn route(self) -> &'static str {
        match self {
            Self::RecordFinding => "record-finding",
            Self::AssessFinding => "assess-finding",
            Self::RecordNote => "record-note",
            Self::ReviewLedger => "list",
            Self::FinishReview => "finish",
            Self::NeedsInput => "needs-input",
        }
    }

    /// Who may call the tool.
    pub const fn caller(self) -> LedgerCaller {
        match self {
            Self::FinishReview | Self::NeedsInput => LedgerCaller::Lead,
            Self::RecordFinding | Self::AssessFinding | Self::RecordNote | Self::ReviewLedger => {
                LedgerCaller::LeadOrSubagent
            }
        }
    }

    /// The description the model reads.
    pub const fn description(self) -> &'static str {
        match self {
            Self::RecordFinding => {
                "Files one confirmed finding of this review, or changes one already on file. Call it as soon as a finding is confirmed: leave out id to file a new finding, or give id and expectedRevision to change only the fields you give. Returns the finding's ID, its revision and how many records wait to be published. A refused call returns the fields to correct and files nothing."
            }
            Self::AssessFinding => {
                "Records what became of a finding already on file, such as one from an earlier version of the report. Call it when you have checked the finding again: fixed, clarified_withdrawn and superseded retire it, while partly_addressed, still_present and unverified keep it active. Returns the finding's ID, its revision and how many records wait to be published. A refused call returns the fields to correct and files nothing."
            }
            Self::RecordNote => {
                "Writes one review note, replacing its earlier text: the report's title, summary, rationale, flow, changes, threads, questions, verification, publication, or an extra note named by its title. Call it whenever a note's text changes: an empty body removes the note, and expectedRevision is needed once the note exists. Returns the note's section, its revision and how many records wait to be published. A refused call returns the fields to correct and files nothing."
            }
            Self::ReviewLedger => {
                "Reads the findings and notes filed for this review, including those not yet published. With no input it returns a short list: the current version, the open review pass and what it still needs, and one line per active finding and note. Give ids or sections, not both, to read those records in full, and pass a returned cursor back with the same input to read the rest. A refused call returns the fields to correct and files nothing."
            }
            Self::FinishReview => {
                "Closes the review and publishes a new version of the report from the findings and notes on file. For the lead agent only, as the last call of its review: give the verdict and revision for a first or full review, or one assessment per selected discussion in a selected-thread check. Returns the version's receipt and which findings are new, updated, retired or carried. A refused call returns the fields to correct and files nothing."
            }
            Self::NeedsInput => {
                "Records a question the reviewer must answer before the review can finish, and lets your turn end without publishing. For the lead agent only, instead of finish_review when you cannot finish, and refused when no review is open. Returns the held review pass, and your next turn continues the review and closes it with finish_review. A refused call returns the fields to correct and files nothing."
            }
        }
    }

    /// The tool's generated input schema, as checked in under `contracts/`.
    pub fn input_schema(self) -> &'static Map<String, Value> {
        TOOL_SCHEMAS
            .get(self.name())
            .and_then(Value::as_object)
            .expect("the generated ledger schemas name every tool")
    }

    /// The tool a host registered as `name`, if it is a ledger tool.
    pub fn from_name(name: &str) -> Option<Self> {
        LEDGER_TOOLS.into_iter().find(|tool| tool.name() == name)
    }
}
