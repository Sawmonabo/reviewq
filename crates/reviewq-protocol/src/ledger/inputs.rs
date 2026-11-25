//! The six tool inputs. Each is one flat object; its doc comments are the descriptions the
//! model reads in the generated schema.

use serde::{Deserialize, Serialize};

use crate::reports::{
    Assessment, FindingBasis, FindingConclusion, FindingLocation, FindingSection, ReportRevision,
    Severity, Verdict,
};
use crate::{DiscussionIdDto, FindingIdDto, GitShaDto, PageCursorDto};

/// Longest finding title or `title` note, in characters.
pub const MAX_TITLE_CHARS: usize = 200;
/// Longest finding category, in characters.
pub const MAX_CATEGORY_CHARS: usize = 48;
/// Longest extra-note title, in characters.
pub const MAX_EXTRA_TITLE_CHARS: usize = 120;
/// Most locations one finding names.
pub const MAX_LOCATIONS: usize = 8;
/// Most finding IDs one `review_ledger` call reads in full.
pub const MAX_READ_IDS: usize = 10;
/// Most discussion IDs one outcome names.
pub const MAX_DISCUSSION_IDS: usize = 10;
/// Most assessments one `finish_review` call carries.
pub const MAX_ASSESSMENTS: usize = 10;
/// Longest finding `details`: 16 KiB of UTF-8.
pub const MAX_DETAILS_BYTES: usize = 16 * 1024;
/// Longest note body: 32 KiB of UTF-8.
pub const MAX_NOTE_BODY_BYTES: usize = 32 * 1024;
/// Largest tool input, as compact JSON: 128 KiB.
pub const MAX_TOOL_INPUT_BYTES: usize = 128 * 1024;

/// How many note sections exist, which bounds one read of `sections`.
#[cfg(feature = "schema")]
const NOTE_SECTIONS: usize = 10;

/// The section a review note fills.
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

/// Files a new finding, or changes the finding named by `id`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RecordFinding {
    /// The finding to change. Leave it out to file a new finding.
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
    #[cfg_attr(feature = "schema", schemars(with = "FindingSection"))]
    pub section: Option<FindingSection>,
    /// A one-line label without `·`, such as Security or Tests. Defaults to the section's name.
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

/// Records what became of a finding already on file.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AssessFinding {
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

/// Replaces one review note.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RecordNote {
    /// `title` is the report's title, on one line. `summary` says what the MR does in at most
    /// four plain paragraphs and `rationale` gives the reason for the verdict in one.
    pub section: NoteSection,
    /// One line naming an `extra` note. Required for `extra` and only for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", length(min = 1, max = MAX_EXTRA_TITLE_CHARS))
    )]
    pub title: Option<String>,
    /// The note's Markdown. An empty body removes the note.
    #[cfg_attr(feature = "schema", schemars(length(max = MAX_NOTE_BODY_BYTES)))]
    pub body: String,
    /// The revision you last read of this note. Required once the note exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "u64", range(min = 1, max = crate::scalar::MAX_JSON_INTEGER))
    )]
    pub expected_revision: Option<u64>,
}

/// Reads the ledger. With no field it returns the short list. Give `ids` or `sections`, not
/// both.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReviewLedger {
    /// Findings to read in full. Leave out `sections` when you give these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schema",
        schemars(length(max = MAX_READ_IDS), extend("uniqueItems" = true))
    )]
    pub ids: Vec<FindingIdDto>,
    /// Note sections to read in full. `extra` reads every extra note. Leave out `ids` when you
    /// give these.
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

/// Closes the review and publishes a version.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FinishReview {
    /// Required for a first version and a full review. Otherwise it defaults to the current
    /// version's verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "Verdict"))]
    pub verdict: Option<Verdict>,
    /// The reviewed commit. Required for a first version and a full review. ReviewQ binds a
    /// selected-thread check's commit itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "ReportRevision"))]
    pub revision: Option<ReportRevision>,
    /// Publish a full review of the whole MR. Every active finding of the last version must
    /// then be changed or assessed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub full_review: bool,
    /// One per selected discussion of a selected-thread check, and only there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "schema", schemars(length(max = MAX_ASSESSMENTS)))]
    pub assessments: Vec<Assessment>,
}

/// Records a question the reviewer must answer before the review can finish.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NeedsInput {
    /// The question, and what the review waits on.
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = crate::reports::MAX_REASON_CHARS)))]
    pub reason: String,
}

/// One validated tool input, as `validate_ledger_input` returns it.
#[derive(Debug)]
pub enum LedgerInput {
    RecordFinding(RecordFinding),
    AssessFinding(AssessFinding),
    RecordNote(RecordNote),
    ReviewLedger(ReviewLedger),
    FinishReview(FinishReview),
    NeedsInput(NeedsInput),
}
