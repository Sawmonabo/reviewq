use reviewq_app::hosts::{DeliveryOutcome, MessagePurpose, RecoverableDeliveryState};

pub(super) fn purpose_label(purpose: MessagePurpose) -> &'static str {
    match purpose {
        MessagePurpose::User => "user",
        MessagePurpose::ReportFeedback => "report_feedback",
        MessagePurpose::Publication => "publication",
        MessagePurpose::ThreadFollowup => "thread_followup",
        MessagePurpose::CodexRecovery => "codex_recovery",
    }
}

pub(super) fn outcome_label(outcome: &DeliveryOutcome) -> &'static str {
    match outcome {
        DeliveryOutcome::Queued => "queued",
        DeliveryOutcome::Dispatching => "dispatching",
        DeliveryOutcome::Started(_) => "started",
        DeliveryOutcome::Rejected => "rejected",
        DeliveryOutcome::Uncertain => "uncertain",
    }
}

pub(super) fn recoverable_label(state: RecoverableDeliveryState) -> &'static str {
    match state {
        RecoverableDeliveryState::Queued => "queued",
        RecoverableDeliveryState::Dispatching => "dispatching",
        RecoverableDeliveryState::Uncertain => "uncertain",
    }
}
