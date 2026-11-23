use reviewq_domain::identity::{
    MessageId, NativeInputId, NativeTurnId, ReceiptId, ReportHash, RunId, SessionKey, SubmissionId,
};

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;

use super::DeliveryState;

pub const MAX_MESSAGE_HISTORY_PAGE: u16 = 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessagePosition {
    pub created_at_ms: i64,
    pub row_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordedPurpose {
    User,
    ReportFeedback,
    CodexRecovery,
    ThreadFollowup,
    Publication,
}

pub struct MessageAttachment {
    pub receipt_id: Option<ReceiptId>,
    pub report_hash: Option<ReportHash>,
    pub current_receipt_id: Option<ReceiptId>,
    pub quote: Option<String>,
    pub quote_offset: Option<u64>,
}

pub struct RecordedMessage {
    pub position: MessagePosition,
    pub message_id: MessageId,
    pub submission_id: SubmissionId,
    pub purpose: RecordedPurpose,
    pub state: DeliveryState,
    pub outcome_detail: Option<String>,
    pub followup_batch_id: Option<String>,
    pub text: String,
    pub attachment: Option<MessageAttachment>,
    pub native_turn_id: Option<NativeTurnId>,
    pub native_entry_id: Option<NativeInputId>,
    pub service_admitted: bool,
    pub updated_at_ms: i64,
}

pub struct MessageHistory {
    pub items: Vec<RecordedMessage>,
    pub has_more: bool,
    pub cursor_message_id: Option<MessageId>,
}

/// Reads the one canonical private message history. Admission and recovery use LeadStore.
pub trait MessageHistoryStore: Send + Sync {
    fn read_messages(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
        after: Option<MessagePosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<MessageHistory>>;
}
