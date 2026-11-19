//! Lead registration, durable admission and claimed native delivery ports.

use reviewq_domain::identity::{
    DeliveryFence, DeliveryId, NativeInputId, NativeSessionId, NativeTurnId, ReceiptId, ReportHash,
    ReviewId, Revision, RunId, SessionKey, SubmissionId,
};
use reviewq_domain::session::{
    AbsolutePath, HostProvider, LeadTarget, NativeBinding, SessionTarget,
};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::history::{HistorySession, NativeHistoryPage, NativePosition};
use crate::queue::ReviewSource;
use crate::sources::{ReviewResolution, ReviewSourceReader};
use crate::workspace::WorkspaceStore;

mod claude;
mod messages;
mod questions;
mod registration;
pub use registration::{authorize_registration, register_proven};
pub mod recovery;
mod work;

pub use claude::{
    ClaudeAnchor, ClaudeChild, ClaudeClassicEvent, ClaudeDeliveryAck, ClaudeDeliveryResult,
    ClaudeHeartbeat, ClaudeHeartbeatResult, ClaudeOffer, ClaudeSend, ClaudeStore, ClaudeTurnResult,
    ClaudeWorkflowContext, ClaudeWorkflowStore, ProvenClaudeChild,
};
pub use messages::{
    MAX_MESSAGE_HISTORY_PAGE, MessageAttachment, MessageHistory, MessageHistoryStore,
    MessagePosition, RecordedMessage, RecordedPurpose,
};
pub use questions::{
    Question, QuestionAnswer, QuestionForm, QuestionOption, QuestionStatus, QuestionsPort,
};
pub use work::{DispatchGuard, receipt, recover_delivery, send_message};

const MAX_MESSAGE_CHARACTERS: usize = 12_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// Maximum size of a startup delivery scan page.
pub const MAX_RECOVERY_LIMIT: u16 = 128;

/// Read-only provider proof of the exact native conversation and working directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostProof {
    pub binding: NativeBinding,
    pub source: Option<AbsolutePath>,
    pub activity: Option<HostActivity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostActivity {
    pub phase: HostPhase,
    pub has_questions: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostPhase {
    Idle,
    Working,
    Blocked,
    Failed,
}

pub trait NativeHost: QuestionsPort + Send + Sync {
    fn prove(&self, native_id: NativeSessionId) -> AppFuture<'_, HostProof>;
    fn history(
        &self,
        target: HistorySession,
        position: NativePosition,
        limit: u16,
    ) -> AppFuture<'_, NativeHistoryPage>;
    fn history_content(
        &self,
        _target: HistorySession,
        _reference: crate::history::RpcContentRef,
        _offset: u64,
        _limit: u32,
    ) -> AppFuture<'_, crate::history::NativeContent> {
        Box::pin(async { Err(AppError::Unavailable) })
    }

    fn history_stamp(
        &self,
        _target: HistorySession,
    ) -> AppFuture<'_, crate::history::NativeRpcStamp> {
        Box::pin(async { Err(AppError::Unavailable) })
    }

    fn history_signals(
        &self,
        _target: HistorySession,
        _file: Option<(AbsolutePath, crate::history::SourceAnchor)>,
        _generation: String,
    ) -> AppFuture<'_, Option<tokio::sync::broadcast::Receiver<crate::history::NativeLiveSignal>>>
    {
        Box::pin(async { Ok(None) })
    }

    fn history_plan(&self, _target: HistorySession) -> AppFuture<'_, crate::history::NativePlan> {
        Box::pin(async { Err(AppError::Unavailable) })
    }

    fn history_navigation(
        &self,
        _target: HistorySession,
        _query: crate::history::NavigationQuery,
    ) -> AppFuture<'_, crate::history::NativeNavigation> {
        Box::pin(async { Err(AppError::Unavailable) })
    }

    fn history_around(
        &self,
        _target: HistorySession,
        _id: NativeInputId,
        _anchor: Option<(NativePosition, String)>,
        _limit: u16,
    ) -> AppFuture<'_, NativeHistoryPage> {
        Box::pin(async { Err(AppError::Unavailable) })
    }

    fn observe_interrupt(&self, _target: LeadTarget, _turn: NativeTurnId) -> AppFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }

    fn interrupt_managed(
        &self,
        _target: LeadTarget,
        _turn: NativeTurnId,
    ) -> AppFuture<'_, recovery::InterruptState> {
        Box::pin(async { Err(AppError::Unavailable) })
    }

    fn dispatch(
        &self,
        target: LeadTarget,
        delivery: DeliveryId,
        text: MessageBody,
        report: Option<ReportRef>,
        guard: DispatchGuard,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DeliveryObservation> + Send + '_>>;
    fn observe(
        &self,
        target: LeadTarget,
        delivery: DeliveryId,
        cursor: Option<String>,
        require_new_turn: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DeliveryObservation> + Send + '_>>;
}

/// Resolves the companion's identity against the service-selected Claude profile.
pub trait ClaudeCompanion: Send + Sync {
    fn accepts_profile(&self, profile: &AbsolutePath) -> bool;
    fn inspect_lead(&self, session: NativeSessionId) -> AppFuture<'_, HostProof>;
    fn prove(&self, context: ClaudeContext) -> AppFuture<'_, HostProof>;
    fn prove_child(
        &self,
        root: HistorySession,
        child: ClaudeChild,
    ) -> AppFuture<'_, ProvenClaudeChild>;
    fn anchor(&self, session: HistorySession) -> AppFuture<'_, ClaudeAnchor>;
    fn confirm_prompt(
        &self,
        session: HistorySession,
        anchor: ClaudeAnchor,
        entry: NativeInputId,
        text: MessageBody,
    ) -> AppFuture<'_, ()>;
}

pub struct ClaudeContext {
    pub session_id: NativeSessionId,
    pub profile: AbsolutePath,
    pub cwd: AbsolutePath,
    pub transcript: Option<AbsolutePath>,
}

/// Opens or displays the terminal for an independently scoped native session.
pub trait NativeTerminal: Send + Sync {
    fn terminal(&self, session: HistorySession, open: bool) -> AppFuture<'_, TerminalHandoff>;
}

pub enum TerminalHandoff {
    Command { command: String, opened: bool },
    Instruction(TerminalInstruction),
}

pub enum TerminalInstruction {
    RestoreCheckout,
    ConfigureProvider,
    ForegroundClaude,
    InspectParent,
}

pub enum NativeLink {
    Linked(Snapshot<LeadTarget>),
    Ambiguous(Vec<String>),
}

/// Registers a verified Codex conversation in the current configured review scope.
pub async fn link_codex(
    store: &(impl WorkspaceStore + LeadStore + ?Sized),
    source: &dyn ReviewSourceReader,
    host: &dyn NativeHost,
    native_id: NativeSessionId,
    target: String,
) -> Result<NativeLink, AppError> {
    let proof = host.prove(native_id).await?;
    if proof.binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::Forbidden);
    }
    link_proven(store, source, proof, target).await
}

/// Links either provider using its independently verified native identity.
pub async fn link_proven(
    store: &(impl WorkspaceStore + LeadStore + ?Sized),
    source: &dyn ReviewSourceReader,
    proof: HostProof,
    target: String,
) -> Result<NativeLink, AppError> {
    let workspace = store
        .workspace_context(Actor::local_operator())
        .await?
        .value;
    let review = match source.resolve(workspace.projects, target).await? {
        ReviewResolution::Found(review) => review,
        ReviewResolution::Ambiguous(urls) => return Ok(NativeLink::Ambiguous(urls)),
    };
    register_proven(store, proof, review, workspace.revision)
        .await
        .map(NativeLink::Linked)
}

/// The provider-verified review and lead to register in one transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterLead {
    pub expected_settings: Revision,
    pub review: ReviewSource,
    pub provider: HostProvider,
    pub profile: AbsolutePath,
    pub native_id: NativeSessionId,
    pub session_key: SessionKey,
    pub cwd: AbsolutePath,
    pub native_source: Option<AbsolutePath>,
    pub activity: Option<HostActivity>,
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

/// A retry may omit the session when its first submission selected a sole lead.
pub struct MessageReplay {
    pub review: ReviewId,
    pub session: Option<SessionKey>,
    pub submission: SubmissionId,
    pub text: MessageBody,
    pub report: Option<ReportRef>,
}

impl SubmitMessage {
    pub fn user_input(
        target: LeadTarget,
        submission_id: SubmissionId,
        text: MessageBody,
        report_ref: Option<ReportRef>,
    ) -> Self {
        Self {
            target,
            submission_id,
            text,
            purpose: if report_ref.is_some() {
                MessagePurpose::ReportFeedback
            } else {
                MessagePurpose::User
            },
            report_ref,
        }
    }
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
    Publication,
    ThreadFollowup,
    CodexRecovery,
}

impl MessagePurpose {
    pub fn requires_new_turn(self) -> bool {
        matches!(
            self,
            Self::Publication | Self::ThreadFollowup | Self::CodexRecovery
        )
    }
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

/// Exclusive position within one bounded pass over currently due intents.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeliveryRecoveryCursor {
    pub due_at_ms: i64,
    pub delivery_id: DeliveryId,
}

/// One candidate discovered without loading its private message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoverableDelivery {
    pub cursor: DeliveryRecoveryCursor,
    pub state: RecoverableDeliveryState,
}

/// Exact immutable report identity pinned by one feedback admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryReport {
    pub receipt_id: ReceiptId,
    pub report_hash: ReportHash,
    pub current_receipt_id: Option<ReceiptId>,
}

/// The stored receipt. The internal delivery ID is never the client retry key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryReceipt {
    pub delivery_id: DeliveryId,
    pub submission_id: SubmissionId,
    pub target: SessionTarget,
    pub outcome: DeliveryOutcome,
    pub report_ref: Option<DeliveryReport>,
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
        workflow: Option<crate::workflow::WorkflowGuard>,
    },
    Reconcile {
        cursor: Option<String>,
    },
}

/// An outcome established by the host adapter for this exact claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryObservation {
    /// The host proved no native input occurred and the same delivery can retry.
    NotSubmitted,
    /// New capacity evidence was acquired before a recovery was sent.
    RecoveryAllowanceChanged(ReportHash),
    /// The input is observable, but starting an independent turn is unproved.
    UncertainInput {
        delivery_id: DeliveryId,
        input: ObservedInput,
    },
    /// A native input provably correlated with this delivery ID.
    Started {
        delivery_id: DeliveryId,
        input: ObservedInput,
    },
    /// The host rejected the input definitively.
    Rejected,
    /// The call might have taken effect, requiring reconciliation before retry.
    Uncertain,
    /// Exact readback can continue at an older native page. No dispatch is authorized.
    ReconcileFrom(String),
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

    /// Reads the existing lead by originated provider/profile/native ID and exact cwd.
    fn lookup_native_lead(
        &self,
        binding: NativeBinding,
    ) -> AppFuture<'_, Snapshot<Option<LeadTarget>>>;

    /// Atomically stores the intent, scoped retry receipt and invalidation.
    fn admit_message(
        &self,
        actor: Actor,
        input: SubmitMessage,
    ) -> AppFuture<'_, Snapshot<DeliveryReceipt>>;

    /// Reads a prior submission before resolving a new recipient among current leads.
    fn replay_message(
        &self,
        actor: Actor,
        input: MessageReplay,
    ) -> AppFuture<'_, Option<Snapshot<DeliveryReceipt>>>;

    /// Reads only the initiating actor's receipt at the original address.
    fn read_receipt(
        &self,
        actor: Actor,
        target: SessionTarget,
        submission_id: SubmissionId,
    ) -> AppFuture<'_, Snapshot<DeliveryReceipt>>;

    /// Scans due dispatch and reconciliation rows in ascending due time and ID order.
    ///
    /// Accepts 1 through `MAX_RECOVERY_LIMIT`. Start at None on startup or a new
    /// reconciliation pass. The last row's cursor continues the current pass.
    /// State is a hint; dispatch must first confirm queued state and acquire a fence.
    fn recoverable_deliveries(
        &self,
        provider: HostProvider,
        after: Option<DeliveryRecoveryCursor>,
        limit: u16,
    ) -> AppFuture<'_, Vec<RecoverableDelivery>>;

    /// Earliest durable due time, including work whose backoff has not elapsed.
    fn next_delivery_due(&self, provider: HostProvider) -> AppFuture<'_, Option<i64>>;

    /// Claims the service delivery ID in the exact expected recoverable state.
    fn claim_delivery(
        &self,
        delivery_id: DeliveryId,
        expected_state: RecoverableDeliveryState,
    ) -> AppFuture<'_, Option<DeliveryClaim>>;

    /// Rechecks the current dispatch fence and authority immediately before native I/O.
    fn authorize_dispatch(&self, claim: DeliveryClaim) -> AppFuture<'_, ()>;

    /// Fresh-source proof requirements for exactly one current automatic claim.
    fn workflow_requirements(
        &self,
        claim: &DeliveryClaim,
    ) -> AppFuture<'_, crate::workflow::WorkflowRequirements>;

    /// Commits a matching claim and fence. Started also requires its observed
    /// delivery ID to match the claim. A stale observation returns conflict.
    fn finish_delivery(
        &self,
        claim: DeliveryClaim,
        observation: DeliveryObservation,
    ) -> AppFuture<'_, DeliveryReceipt>;
}
