//! Emit synthetic receipt envelopes using the same Serde DTOs as transport.

use std::error::Error;
use std::io::{self, Write};

use reviewq_protocol::{
    DeliveryReceipt, DeliveryReceiptResponse, DeliveryState, ErrorCode, ErrorDto, EventCursorDto,
    Failure, RunIdDto, SessionKeyDto, SubmissionIdDto, UtcTimestampDto,
};

fn main() -> Result<(), Box<dyn Error>> {
    assert!(UtcTimestampDto::new("2026-02-31T00:00:00.000Z").is_none());
    let response = DeliveryReceiptResponse {
        data: DeliveryReceipt {
            submission_id: SubmissionIdDto::new("send_42").unwrap(),
            run_id: RunIdDto::new("run_active").unwrap(),
            session_key: SessionKeyDto::new("codex:active").unwrap(),
            state: DeliveryState::Queued { observed_input: () },
            created_at: UtcTimestampDto::new("2026-11-01T00:00:00.000Z").unwrap(),
            updated_at: UtcTimestampDto::new("2026-11-01T00:00:00.000Z").unwrap(),
        },
        cursor: EventCursorDto::new(9_007_199_254_740_993).unwrap(),
    };
    let failure = Failure {
        error: ErrorDto {
            code: ErrorCode::NotFound,
            message: "Message receipt unavailable.".into(),
            retryable: false,
        },
        cursor: None,
    };

    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, &response)?;
    writeln!(output)?;
    serde_json::to_writer(&mut output, &failure)?;
    writeln!(output)?;
    Ok(())
}
