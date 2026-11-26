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
                "Files one confirmed finding of this review, or changes one already on file. Call it as soon as a finding is confirmed. A new finding leaves out id and expectedRevision and must give title, severity, blocking, section, issue, consequence and evidence; category, locations, proposedComment and details are optional. A change gives id, the expectedRevision you last read, and only the fields it changes. Returns the finding's ID, its revision and how many records wait to be published. A refused call files nothing and says what to correct."
            }
            Self::AssessFinding => {
                "Records what became of a finding already on file, such as one from an earlier version of the report, once you have checked it again. Give its id, the expectedRevision you last read, an outcome and a one-line reason; discussionIds and checkedSha are optional. fixed, clarified_withdrawn and superseded retire the finding, while partly_addressed, still_present and unverified keep it active. A full review must assess or change every finding that is active on the current version. Returns the finding's ID, its revision and how many records wait to be published. A refused call files nothing and says what to correct."
            }
            Self::RecordNote => {
                "Writes one review note, replacing its earlier text. Give section and body: section is title, summary, rationale, flow, changes, threads, questions, verification, publication or extra, and an extra note also needs a title, which no other note takes. Leave out expectedRevision when you first write a note, and give the revision you last read once it exists. An empty body removes the note. Returns the note's section, its revision and how many records wait to be published. A refused call files nothing and says what to correct."
            }
            Self::ReviewLedger => {
                "Reads the findings and notes filed for this review, including those not yet published. With no input it returns a short list: the current version, the open review pass and what it still needs, and one line per active finding and note with its revision. To read records in full, give ids (at most 10) or sections, not both, and pass a returned cursor back with the same input to read the rest."
            }
            Self::FinishReview => {
                "Closes the review and publishes a new version of the report from the findings and notes on file; while a review is open, only this call shows the reviewer what was filed. For the lead agent only. A first or full review needs verdict and revision: {\"sha\":\"<full SHA of the reviewed commit>\"}, or {\"sha\":null,\"reason\":\"...\"} when it is unknown. A full review of a report that already has a version also needs each finding active on that version assessed with assess_finding or changed first. A selected-thread check needs one assessment per selected discussion and takes no fullReview; ReviewQ sets its revision, and verdict is optional. With no review open, verdict and revision default to the current version's, and fullReview:true makes the call a full review. Returns the version's receipt and which findings are new, updated, retired or carried. A refused call says what to do next."
            }
            Self::NeedsInput => {
                "Holds the review when it cannot finish until the reviewer answers a question, so your turn can end without closing it. For the lead agent only, and refused when no review is open. Give the question as reason and ask it in your reply as well: ReviewQ tells the reviewer only that the review needs input. It publishes nothing and keeps everything on file; once the reviewer answers, close the review with finish_review. Returns the held review pass and how many records wait to be published."
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
