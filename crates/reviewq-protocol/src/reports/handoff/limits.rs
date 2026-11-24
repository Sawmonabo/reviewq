//! Bounds of the report handoff. The generated schema and the value rules share them.

pub const MAX_FINDINGS: usize = 100;
pub const MAX_FINDING_CHANGES: usize = 100;
pub const MAX_ASSESSMENTS: usize = 10;
pub const MAX_ASSESSMENT_FINDING_IDS: usize = 100;
pub const MAX_DISCUSSION_IDS: usize = 10;

pub const MAX_ID_BYTES: usize = 64;
pub const MAX_DISCUSSION_ID_BYTES: usize = 256;
pub const MAX_TITLE_BYTES: usize = 384;
pub const MAX_SEVERITY_BYTES: usize = 64;
pub const MAX_CONSEQUENCE_BYTES: usize = 1024;
pub const MAX_EVIDENCE_BYTES: usize = 2048;
pub const MAX_PATH_BYTES: usize = 512;
pub const MAX_REASON_BYTES: usize = 1024;
