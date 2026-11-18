//! Lead registration, durable admission and claimed native delivery ports.

use reviewq_domain::identity::{
    DeliveryFence, DeliveryId, NativeInputId, NativeSessionId, NativeTurnId, ReceiptId, ReportHash,
    RunId, SessionKey, SubmissionId,
};
use reviewq_domain::session::{AbsolutePath, HostProvider, LeadTarget, SessionTarget};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::queue::ReviewSource;

const MAX_MESSAGE_CHARACTERS: usize = 12_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// Maximum size of a startup delivery scan page.
pub const MAX_RECOVERY_LIMIT: u16 = 128;

/// The provider-verified review and lead to register in one transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterLead {
    pub review: ReviewSource,
    pub provider: HostProvider,
    pub profile: AbsolutePath,
    pub native_id: NativeSessionId,
    pub session_key: SessionKey,
    pub cwd: AbsolutePath,
}

/// A user message admitted to the native outbox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmitMessage {
    pub target: LeadTarget,
    pub submission_id: SubmissionId,
    pub text: MessageBody,
    pub purpose: MessagePurpose,
    pub report_ref: Option<ReportRef>,
}

/// The original, bounded user message. Whitespace is preserved byte-for-byte.
#[derive(Clone, Eq, PartialEq)]
pub struct MessageBody(Box<str>);

impl MessageBody {
    pub fn new(value: &str) -> Result<Self, AppError> {
        if value.is_empty()
            || value.chars().take(MAX_MESSAGE_CHARACTERS + 1).count() > MAX_MESSAGE_CHARACTERS
        {
            return Err(AppError::InvalidRequest);
        }
        Ok(Self(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for MessageBody {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("MessageBody(<private>)")
    }
}

/// The purpose of a durable lead message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessagePurpose {
    User,
    ReportFeedback,
}

/// The displayed immutable report referenced by a message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportRef {
    pub receipt_id: ReceiptId,
    pub report_hash: ReportHash,
    pub quote: Option<ReportQuote>,
}

/// Exact nonempty source text at a Unicode scalar range in the displayed report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportQuote {
    text: MessageBody,
    offset: u64,
}

impl ReportQuote {
    /// Bounds the addressed range without changing the supplied source bytes.
    pub fn new(text: MessageBody, offset: u64) -> Result<Self, AppError> {
        let length = text.as_str().chars().count() as u64;
        if offset > MAX_SAFE_INTEGER - length {
            return Err(AppError::InvalidRequest);
        }
        Ok(Self { text, offset })
    }

    pub fn text(&self) -> &MessageBody {
        &self.text
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn length(&self) -> u64 {
        self.text.as_str().chars().count() as u64
    }
}

/// The durable state used by the send and lookup operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryState {
    Queued,
    Dispatching,
    Started,
    Rejected,
    Uncertain,
}

/// Distinguishes safe dispatch from states requiring native reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoverableDeliveryState {
    Queued,
    Dispatching,
    Uncertain,
}

/// Stable ascending key of one outbox row, independent of state changes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeliveryRecoveryCursor {
    pub created_at_ms: i64,
    pub delivery_id: DeliveryId,
}

/// One candidate discovered without loading its private message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoverableDelivery {
    pub cursor: DeliveryRecoveryCursor,
    pub state: RecoverableDeliveryState,
}

/// The stored receipt. The internal delivery ID is never the client retry key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryReceipt {
    pub delivery_id: DeliveryId,
    pub submission_id: SubmissionId,
    pub target: SessionTarget,
    pub outcome: DeliveryOutcome,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// An exact native input address within the receipt's run/session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedInput {
    pub turn_id: NativeTurnId,
    pub entry_id: NativeInputId,
}

/// The recorded result. Started always includes the input that proves it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    Queued,
    Dispatching,
    Started(ObservedInput),
    Rejected,
    Uncertain,
}

/// Exact accepted delivery, attempt fence and native binding checked before dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryClaim {
    pub delivery_id: DeliveryId,
    pub fence: DeliveryFence,
    pub target: LeadTarget,
    pub purpose: MessagePurpose,
    pub work: DeliveryWork,
}

/// Only an exact queued claim carries input authorized for native dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryWork {
    Dispatch {
        text: MessageBody,
        report_ref: Option<ReportRef>,
    },
    Reconcile,
}

/// An outcome established by the host adapter for this exact claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryObservation {
    /// The host proved no native input occurred and the same delivery can retry.
    NotSubmitted,
    /// A native input provably correlated with this delivery ID.
    Started {
        delivery_id: DeliveryId,
        input: ObservedInput,
    },
    /// The host rejected the input definitively.
    Rejected,
    /// The call might have taken effect, requiring reconciliation before retry.
    Uncertain,
}

/// Transactional operations for verified registration and message delivery.
pub trait LeadStore: Send + Sync {
    /// Returns the existing lead for an identical proven registration.
    fn register_lead(
        &self,
        actor: Actor,
        input: RegisterLead,
    ) -> AppFuture<'_, Snapshot<LeadTarget>>;

    /// Rechecks actor scope and confirms the exact run/session is its lead.
    fn lookup_lead(
        &self,
        actor: Actor,
        run_id: RunId,
        session_key: SessionKey,
    ) -> AppFuture<'_, Snapshot<LeadTarget>>;

    /// Atomically stores the intent, scoped retry receipt and invalidation.
    fn admit_message(
        &self,
        actor: Actor,
        input: SubmitMessage,
    ) -> AppFuture<'_, Snapshot<DeliveryReceipt>>;

    /// Reads only the initiating actor's receipt at the original address.
    fn read_receipt(
        &self,
        actor: Actor,
        target: SessionTarget,
        submission_id: SubmissionId,
    ) -> AppFuture<'_, Snapshot<DeliveryReceipt>>;

    /// Scans recoverable rows after an exclusive key in ascending creation/ID order.
    ///
    /// Accepts 1 through `MAX_RECOVERY_LIMIT`. Start at None on startup or a new
    /// reconciliation pass. The last row's cursor continues the current pass.
    /// State is a hint; dispatch must first confirm queued state and acquire a fence.
    fn recoverable_deliveries(
        &self,
        after: Option<DeliveryRecoveryCursor>,
        limit: u16,
    ) -> AppFuture<'_, Vec<RecoverableDelivery>>;

    /// Claims the service delivery ID in the exact expected recoverable state.
    fn claim_delivery(
        &self,
        delivery_id: DeliveryId,
        expected_state: RecoverableDeliveryState,
    ) -> AppFuture<'_, Option<DeliveryClaim>>;

    /// Rechecks the current dispatch fence and authority immediately before native I/O.
    fn authorize_dispatch(&self, claim: DeliveryClaim) -> AppFuture<'_, ()>;

    /// Commits a matching claim and fence. Started also requires its observed
    /// delivery ID to match the claim. A stale observation returns conflict.
    fn finish_delivery(
        &self,
        claim: DeliveryClaim,
        observation: DeliveryObservation,
    ) -> AppFuture<'_, DeliveryReceipt>;
}
