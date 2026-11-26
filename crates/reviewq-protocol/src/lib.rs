//! Versioned JSON contracts for ReviewQ transport adapters.

pub mod actions;
pub mod changes;
mod common;
pub mod deadlines;
pub mod decisions;
pub mod events;
pub mod field_errors;
pub mod followups;
pub mod history;
mod hosts;
pub mod launch;
pub mod ledger;
pub mod mcp;
pub mod notifications;
pub mod questions;
pub mod queue;
pub mod recovery;
pub mod registration;
pub mod reports;
mod scalar;
pub mod settings;

pub use common::{ErrorCode, ErrorDto, Failure, Page, Success, UtcTimestampDto};
pub use hosts::{
    DeliveryReceipt, DeliveryReceiptResponse, DeliveryReportRef, DeliveryState, HostProviderDto,
    MessageAttachment, MessageHistoryItem, MessageHistoryPage, MessageTextDto,
    NativeSessionMetadata, ObservedInputDto, RecordedPurpose, RecordedState, RunDetail,
    RunDetailResponse, SendMessageRequest, SessionSummary, TerminalHandoff, TerminalResponse,
    TerminalResult,
};
pub use scalar::{
    CheckpointIdDto, DiscussionIdDto, EventCursorDto, FileIdDto, FindingIdDto, GitShaDto,
    HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, HistoryTurnIdDto,
    MergeRequestIidDto, PageCursorDto, PatchETagDto, PatchRevisionDto, ProjectIdDto, ResourceIdDto,
    ReviewIdDto, RevisionDto, RunIdDto, SafeCountDto, SessionKeyDto, SubmissionIdDto, TicketKeyDto,
    UpstreamIdDto,
};
