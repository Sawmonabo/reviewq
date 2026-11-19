//! Versioned JSON contracts for ReviewQ transport adapters.

pub mod changes;
mod common;
pub mod decisions;
pub mod events;
pub mod history;
mod hosts;
pub mod mcp;
pub mod notifications;
pub mod queue;
pub mod reports;
mod scalar;

pub use common::{ErrorCode, ErrorDto, Failure, Page, Success, UtcTimestampDto};
pub use hosts::{
    DeliveryReceipt, DeliveryReceiptResponse, DeliveryState, HostProviderDto, MessageAttachment,
    MessageHistoryItem, MessageHistoryPage, MessageTextDto, ObservedInputDto, RecordedPurpose,
    RecordedState, RunDetail, RunDetailResponse, SendMessageRequest, SessionSummary,
};
pub use scalar::{
    CheckpointIdDto, EventCursorDto, FileIdDto, GitShaDto, HistoryCursorDto, HistoryEntryIdDto,
    HistoryGenerationDto, HistoryTurnIdDto, MergeRequestIidDto, PageCursorDto, PatchETagDto,
    PatchRevisionDto, ProjectIdDto, ResourceIdDto, ReviewIdDto, RevisionDto, RunIdDto,
    SafeCountDto, SessionKeyDto, SubmissionIdDto, TicketKeyDto, UpstreamIdDto,
};
