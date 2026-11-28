//! The six reviewing tool inputs. Each is one flat object; its doc comments are the descriptions the
//! model reads in the generated schema.

use serde::{Deserialize, Serialize};

use crate::reports::{
    Assessment, FindingBasis, FindingConclusion, FindingKind, FindingLocation, ReportRevision,
    Severity,
};
use crate::{DiscussionIdDto, FindingIdDto, GitShaDto, PageCursorDto};

/// Longest finding title or `title` section, in characters.
pub const MAX_TITLE_CHARS: usize = 200;
/// Longest finding category, in characters.
pub const MAX_CATEGORY_CHARS: usize = 48;
/// Longest title of an `extra` section, in characters.
pub const MAX_EXTRA_TITLE_CHARS: usize = 120;
/// Most locations one finding names.
pub const MAX_LOCATIONS: usize = 8;
/// Most finding IDs one `read_draft` call reads in full.
pub const MAX_READ_IDS: usize = 10;
/// Most discussion IDs one outcome names.
pub const MAX_DISCUSSION_IDS: usize = 10;
/// Most assessments one `submit_review` call carries.
pub const MAX_ASSESSMENTS: usize = 10;
/// Longest finding `details`: 16 KiB of UTF-8.
pub const MAX_DETAILS_BYTES: usize = 16 * 1024;
/// Longest section body: 32 KiB of UTF-8.
pub const MAX_NOTE_BODY_BYTES: usize = 32 * 1024;
/// Largest tool input, as compact JSON: 128 KiB.
pub const MAX_TOOL_INPUT_BYTES: usize = 128 * 1024;

/// How many report sections `save_section` writes, which bounds one read of `sections`.
#[cfg(feature = "schema")]
const NOTE_SECTIONS: usize = 10;

/// A section of the report, which `save_section` writes.
// A note is the stored text of one section: `NoteSection` names it and `ledger_notes` keeps it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum NoteSection {
    Title,
    Summary,
    Rationale,
    Flow,
    Changes,
    Threads,
    Questions,
    Verification,
    Publication,
    Extra,
}

/// Saves a new finding, or changes the finding named by `id`. A new finding gives `title`,
/// `severity`, `blocking`, `kind`, `issue`, `consequence` and `evidence`. A change gives
/// `id`, `expectedRevision` and only the fields it changes.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SaveFinding {
    /// The finding to change. Leave it out to save a new finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "FindingIdDto"))]
    pub id: Option<FindingIdDto>,
    /// The revision you last read of the finding named by `id`. Required with `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "u64", range(min = 1, max = crate::scalar::MAX_JSON_INTEGER))
    )]
    pub expected_revision: Option<u64>,
    /// One line naming the problem, without a severity label such as `P1 –`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", length(min = 1, max = MAX_TITLE_CHARS))
    )]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "Severity"))]
    pub severity: Option<Severity>,
    /// Whether the lead holds the merge on this finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "bool"))]
    pub blocking: Option<bool>,
    /// `functional` or `quality`. Security and correctness concerns are functional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "FindingKind"))]
    pub kind: Option<FindingKind>,
    /// A one-line label without `·`, such as Security or Tests. Defaults to the kind's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", length(min = 1, max = MAX_CATEGORY_CHARS))
    )]
    pub category: Option<String>,
    /// The files the finding points at. An empty list removes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Vec<FindingLocation>", length(max = MAX_LOCATIONS))
    )]
    pub locations: Option<Vec<FindingLocation>>,
    /// Markdown starting with a paragraph: what is wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", length(min = 1, max = crate::reports::MAX_FINDING_TEXT_BYTES))
    )]
    pub issue: Option<String>,
    /// Markdown starting with a paragraph: why it matters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", length(min = 1, max = crate::reports::MAX_FINDING_TEXT_BYTES))
    )]
    pub consequence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "FindingBasis"))]
    pub evidence: Option<FindingBasis>,
    /// Markdown starting with a paragraph: the comment the lead would post. An empty string
    /// removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", length(max = crate::reports::MAX_FINDING_TEXT_BYTES))
    )]
    pub proposed_comment: Option<String>,
    /// Markdown for diagrams, tables and longer reasoning, shown after the evidence. An empty
    /// string removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", length(max = MAX_DETAILS_BYTES))
    )]
    pub details: Option<String>,
}

/// Sets what became of a finding already saved.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SetFindingOutcome {
    pub id: FindingIdDto,
    /// The revision you last read of this finding.
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 1, max = crate::scalar::MAX_JSON_INTEGER))
    )]
    pub expected_revision: u64,
    /// `fixed`, `clarified_withdrawn` and `superseded` retire the finding. The others keep it
    /// active.
    pub outcome: FindingConclusion,
    /// One line saying what you checked.
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = crate::reports::MAX_REASON_CHARS)))]
    pub reason: String,
    /// The GitLab discussions this outcome answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schema",
        schemars(length(max = MAX_DISCUSSION_IDS), extend("uniqueItems" = true))
    )]
    pub discussion_ids: Vec<DiscussionIdDto>,
    /// The commit you checked, when it differs from the reviewed revision of the closing call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "GitShaDto"))]
    pub checked_sha: Option<GitShaDto>,
}

/// Replaces one section of the report.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SaveSection {
    /// `title` is the report's title, on one line. `summary` says what the MR does in at most
    /// four plain paragraphs and `rationale` gives the reason for the verdict in one. `flow` is
    /// a Mermaid diagram of the principal code and data flow in a fenced block that opens with
    /// a line ```mermaid, then a short explanation. `changes` is a table with the two columns
    /// Before and After, then a table with the columns Affected operation, What changes and
    /// Source. `threads`, `questions`, `verification` and `publication` fill the report
    /// sections of those names, and `extra` adds a section under its own title. A first
    /// version or a full review needs `summary`, `rationale`, `flow`, `changes` and
    /// `verification`, and once saved they cannot be removed.
    pub section: NoteSection,
    /// One line naming an `extra` section. Required for `extra` and only for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", length(min = 1, max = MAX_EXTRA_TITLE_CHARS))
    )]
    pub title: Option<String>,
    /// The section's Markdown. An empty body removes the section, except a required one.
    #[cfg_attr(feature = "schema", schemars(length(max = MAX_NOTE_BODY_BYTES)))]
    pub body: String,
    /// The revision you last read of this section. Required once the section exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "u64", range(min = 1, max = crate::scalar::MAX_JSON_INTEGER))
    )]
    pub expected_revision: Option<u64>,
}

/// Reads the saved findings and sections. With no field it returns the short list. Give `ids`
/// or `sections`, not both.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReadDraft {
    /// Findings to read in full. Leave out `sections` when you give these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schema",
        schemars(length(max = MAX_READ_IDS), extend("uniqueItems" = true))
    )]
    pub ids: Vec<FindingIdDto>,
    /// Sections to read in full. `extra` reads every extra section. Leave out `ids` when you give
    /// these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schema",
        schemars(length(max = NOTE_SECTIONS), extend("uniqueItems" = true))
    )]
    pub sections: Vec<NoteSection>,
    /// The cursor a previous result returned. Send it with the same `ids` or `sections`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "PageCursorDto"))]
    pub cursor: Option<PageCursorDto>,
}

/// Closes the review and publishes a version. A first or full review needs `verdict`,
/// `revision` and the `summary`, `rationale`, `flow`, `changes` and `verification` sections; a
/// selected-thread check needs one entry in `assessments` per selected discussion.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SubmitReview {
    /// Required for a first version and a full review. Otherwise it defaults to the current
    /// version's verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "ClosingVerdict"))]
    pub verdict: Option<ClosingVerdict>,
    /// The reviewed commit. Required for a first version and a full review. ReviewQ binds a
    /// selected-thread check's commit itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "ReportRevision"))]
    pub revision: Option<ReportRevision>,
    /// Publish a full review of the whole MR. Every active finding of the last version must
    /// then be changed or given an outcome.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub full_review: bool,
    /// One per selected discussion of a selected-thread check, and only there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "schema", schemars(length(max = MAX_ASSESSMENTS)))]
    pub assessments: Vec<Assessment>,
}

// A stored report can also read `not_specified`, as imported ones do, but a closing call always
// states one of these three, so the schema offers no other.
/// The verdict of the review.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ClosingVerdict {
    Approve,
    RequestChanges,
    Blocked,
}

/// Pauses the review until the user replies to a question that must be answered before the
/// review can finish. It publishes nothing.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WaitForUser {
    /// The question, and what the review waits on. Ask it in your reply as well: the user is
    /// told only that the review needs input.
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = crate::reports::MAX_REASON_CHARS)))]
    pub question: String,
}

/// One validated tool input, as `validate_ledger_input` returns it.
#[derive(Debug)]
pub enum LedgerInput {
    SaveFinding(SaveFinding),
    SetFindingOutcome(SetFindingOutcome),
    SaveSection(SaveSection),
    ReadDraft(ReadDraft),
    SubmitReview(SubmitReview),
    WaitForUser(WaitForUser),
}
