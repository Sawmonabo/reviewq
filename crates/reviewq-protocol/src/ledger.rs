//! The reviewing tools' contract: the six tools, their inputs and results, the native route
//! bodies, and the one validator every entry point calls.

use std::sync::LazyLock;

use serde_json::{Map, Value};

mod inputs;
mod results;
mod routes;
mod validate;

pub use inputs::{
    ClosingVerdict, LedgerInput, MAX_ASSESSMENTS, MAX_CATEGORY_CHARS, MAX_DETAILS_BYTES,
    MAX_DISCUSSION_IDS, MAX_EXTRA_TITLE_CHARS, MAX_LOCATIONS, MAX_NOTE_BODY_BYTES, MAX_READ_IDS,
    MAX_TITLE_CHARS, MAX_TOOL_INPUT_BYTES, NoteSection, ReadDraft, SaveFinding, SaveSection,
    SetFindingOutcome, SubmitReview, WaitForUser,
};
pub use results::{
    FindingLine, FindingReceipt, FindingRecord, HeldReceipt, LedgerPage, LedgerPass, LedgerRecords,
    LedgerSummary, NoteLine, NoteReceipt, NoteRecord, PassKind, RecordedOutcome, SubmitReceipt,
};
pub use routes::{
    GateReply, GateRequest, LedgerBinding, LedgerRequest, ReadbackReply, ReadbackRequest,
};
#[cfg(feature = "schema")]
pub use validate::validate_against;
pub use validate::{oversized_input, validate_ledger_input};

/// The generated input schemas, one self-contained Draft 7 schema per tool name.
const SCHEMAS: &str = include_str!("../../../contracts/ledger-tools.schema.json");

static TOOL_SCHEMAS: LazyLock<Map<String, Value>> = LazyLock::new(|| {
    serde_json::from_str(SCHEMAS).expect("the generated ledger tool schemas are a JSON object")
});

/// One of the six reviewing tools.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LedgerTool {
    SaveFinding,
    SetFindingOutcome,
    SaveSection,
    ReadDraft,
    SubmitReview,
    WaitForUser,
}

/// Largest JSON text of one tool's result, the text the model reads, on both hosts. A
/// `read_draft` page is the largest result. The store budgets a page at 64 KiB of the
/// UTF-8 text of its records, or one record alone when it is larger, at most about 80 KiB;
/// escaping a control character as `\u00XX` grows that text at most sixfold, to 480 KiB. The
/// rest of a page is not budgeted but is bounded: at most 200 finding lines and 21 note lines,
/// or 10 findings and 21 notes, each with an ID and author of at most 512 bytes, under 300 KiB
/// escaped. So a full page always fits, and a refusal, at most 51 short field errors, does too.
pub const MAX_LEDGER_RESULT_BYTES: usize = 1024 * 1024;

/// Every reviewing tool, in the order hosts list them.
pub const LEDGER_TOOLS: [LedgerTool; 6] = [
    LedgerTool::SaveFinding,
    LedgerTool::SetFindingOutcome,
    LedgerTool::SaveSection,
    LedgerTool::ReadDraft,
    LedgerTool::SubmitReview,
    LedgerTool::WaitForUser,
];

impl LedgerTool {
    /// The tool name both hosts register.
    pub const fn name(self) -> &'static str {
        match self {
            Self::SaveFinding => "save_finding",
            Self::SetFindingOutcome => "set_finding_outcome",
            Self::SaveSection => "save_section",
            Self::ReadDraft => "read_draft",
            Self::SubmitReview => "submit_review",
            Self::WaitForUser => "wait_for_user",
        }
    }

    /// The tool's route segment under `/api/v1/native/ledger/`: its name with hyphens.
    pub fn route(self) -> String {
        self.name().replace('_', "-")
    }

    /// The description the model reads.
    pub const fn description(self) -> &'static str {
        match self {
            Self::SaveFinding => {
                "Saves one confirmed finding of this review, or changes a finding already saved. Call it as soon as a finding is confirmed rather than at the end. A new finding leaves out id and expectedRevision and must give title, severity, blocking, kind, issue, consequence and evidence; category, locations, proposedComment and details are optional. A change gives id, the expectedRevision you last read, and only the fields it changes. Saving posts nothing to GitLab. Returns the finding's ID, its revision and how many findings and sections changed since the last published version. A refused call saves nothing and says what to correct."
            }
            Self::SetFindingOutcome => {
                "Sets what became of a finding already saved, such as one from an earlier version of the report, once you have checked it again. Give its id, the expectedRevision you last read, an outcome and a one-line reason; discussionIds and checkedSha are optional. fixed, clarified_withdrawn and superseded retire the finding, while partly_addressed, still_present and unverified keep it active. A full review must set an outcome for, or change with save_finding, every finding that is active on the current version. Returns the finding's ID, its revision and how many findings and sections changed since the last published version. A refused call saves nothing and says what to correct."
            }
            Self::SaveSection => {
                "Writes one section of the report, replacing its earlier text. Give section and body. title is the report's title; summary says what the MR does; rationale gives the reason for the verdict; flow is a Mermaid diagram of the principal code and data flow in a fenced block that opens with a line ```mermaid, then a short explanation, and is refused without a diagram the page can draw; changes is a table with the two columns Before and After, then a table with the columns Affected operation, What changes and Source, and is refused without both tables; threads says what became of earlier review threads; questions holds open questions and decisions; verification says what you checked and what you could not; publication says which comments were posted or held back; extra is any other section, under its own title, that no other section covers. A first or full review cannot be submitted until the summary, rationale, flow, changes and verification sections are saved, and once saved they cannot be removed. Leave out expectedRevision when you first write a section, and give the revision you last read once it exists. An empty body removes any other section. Returns the section, its revision and how many findings and sections changed since the last published version. A refused call saves nothing and says what to correct."
            }
            Self::ReadDraft => {
                "Reads the findings and sections saved for this review, including those not yet published. With no input it returns a short list: the current version, the open review pass and what it still needs, one line per active finding under findings and one per saved section under sections, each with its revision. To read them in full, give ids (at most 10) or sections, not both, and pass a returned cursor back with the same input to read the rest."
            }
            Self::SubmitReview => {
                "Closes the review and publishes a new version of the report from the saved findings and sections; while a review is open, only this call shows the user what was saved. For the lead agent only. A first or full review needs verdict, which is approve, request_changes or blocked, and revision: {\"sha\":\"<full SHA of the reviewed commit>\"}, or {\"sha\":null,\"reason\":\"...\"} when it is unknown. A first or full review also needs the summary, rationale, flow, changes and verification sections, so save them with save_section first. A full review of a report that already has a version also needs each finding active on that version changed with save_finding or given an outcome with set_finding_outcome first. A selected-thread check needs one assessment per selected discussion and takes no fullReview; ReviewQ sets its revision, and verdict is optional. With no review open, verdict and revision default to the current version's, and fullReview:true makes the call a full review. Returns the version's receipt and which findings are new, updated, retired or carried. A refused call says what to do next."
            }
            Self::WaitForUser => {
                "Pauses the review until the user replies, so your turn can end without submitting it. ReviewQ tells the user only that the review needs input, so give your question as question and also ask it in your reply. This is not your host's own question prompt: it does not ask the user anything for you. For the lead agent only, and refused when no review is open. It publishes nothing and keeps everything saved; once the user replies, submit the review with submit_review. Returns the paused review pass and how many findings and sections changed since the last published version."
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

    /// The tool a host registered as `name`, if it is a reviewing tool.
    pub fn from_name(name: &str) -> Option<Self> {
        LEDGER_TOOLS.into_iter().find(|tool| tool.name() == name)
    }
}
