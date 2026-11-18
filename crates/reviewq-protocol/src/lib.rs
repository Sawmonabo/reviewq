//! Versioned JSON contracts for ReviewQ transport adapters.

mod common;
mod hosts;

pub use common::{ErrorCode, ErrorDto, EventCursorDto, Failure, Success, UtcTimestampDto};
pub use hosts::{
    DeliveryReceipt, DeliveryReceiptResponse, DeliveryState, RunIdDto, SessionKeyDto,
    SubmissionIdDto,
};
