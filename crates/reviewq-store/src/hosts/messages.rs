use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::hosts::{
    DeliveryState, MAX_MESSAGE_HISTORY_PAGE, MessageAttachment, MessageHistory,
    MessageHistoryStore, MessagePosition, RecordedMessage, RecordedPurpose,
};
use reviewq_domain::identity::{RunId, SessionKey};
use rusqlite::{Connection, Row, params};

use crate::events::snapshot;
use crate::queue::{parse, text};
use crate::reports::authorize_run;
use crate::{SqliteStore, StoreError};

impl MessageHistoryStore for SqliteStore {
    fn read_messages(
        &self,
        actor: Actor,
        run: RunId,
        session: SessionKey,
        after: Option<MessagePosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<MessageHistory>> {
        Box::pin(async move {
            if !(1..=MAX_MESSAGE_HISTORY_PAGE).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| {
                authorize_run(tx, &actor, &run)?;
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE run_id=?1 AND session_key=?2)",
                    params![run.as_str(), session.as_str()],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(AppError::NotFound.into());
                }
                snapshot(tx, read_page(tx, &run, &session, after, limit)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn read_page(
    db: &Connection,
    run: &RunId,
    session: &SessionKey,
    after: Option<MessagePosition>,
    limit: u16,
) -> Result<MessageHistory, StoreError> {
    let (time, id) = after.as_ref().map_or((None, ""), |p| {
        (Some(p.created_at_ms), p.message_id.as_str())
    });
    let mut statement = db.prepare_cached(
        "SELECT message_id, submission_id, purpose, state, body, receipt_id, report_hash,
            current_receipt_id, quoted_text, quoted_offset, native_turn_id, native_input_id,
            actor_key IS NOT NULL, created_at_ms, updated_at_ms
         FROM messages
         WHERE run_id=?1 AND session_key=?2 AND (?3 IS NULL OR (created_at_ms, message_id)<(?3,?4))
         ORDER BY created_at_ms DESC, message_id DESC LIMIT ?5",
    )?;
    let mut rows = statement.query(params![run.as_str(), session.as_str(), time, id, limit + 1])?;
    let mut items = Vec::with_capacity(usize::from(limit));
    while let Some(row) = rows.next()? {
        if items.len() == usize::from(limit) {
            return Ok(MessageHistory {
                items,
                has_more: true,
            });
        }
        items.push(decode(row)?);
    }
    Ok(MessageHistory {
        items,
        has_more: false,
    })
}

fn decode(row: &Row<'_>) -> Result<RecordedMessage, StoreError> {
    let attachment = MessageAttachment {
        receipt_id: optional(row, 5)?,
        report_hash: optional(row, 6)?,
        current_receipt_id: optional(row, 7)?,
        quote: row.get(8)?,
        quote_offset: row
            .get::<_, Option<i64>>(9)?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| AppError::Internal)?,
    };
    let has_attachment = attachment.receipt_id.is_some()
        || attachment.report_hash.is_some()
        || attachment.current_receipt_id.is_some()
        || attachment.quote.is_some();
    let purpose = match text(row, 2)? {
        "user" => RecordedPurpose::User,
        "report_feedback" => RecordedPurpose::ReportFeedback,
        "codex_recovery" => RecordedPurpose::CodexRecovery,
        "thread_followup" => RecordedPurpose::ThreadFollowup,
        "publication" => RecordedPurpose::Publication,
        _ => return Err(AppError::Internal.into()),
    };
    let state = match text(row, 3)? {
        "queued" => DeliveryState::Queued,
        "dispatching" => DeliveryState::Dispatching,
        "started" => DeliveryState::Started,
        "rejected" => DeliveryState::Rejected,
        "uncertain" => DeliveryState::Uncertain,
        _ => return Err(AppError::Internal.into()),
    };
    Ok(RecordedMessage {
        position: MessagePosition {
            created_at_ms: row.get(13)?,
            message_id: parse(text(row, 0)?)?,
        },
        submission_id: parse(text(row, 1)?)?,
        purpose,
        state,
        text: row.get(4)?,
        attachment: has_attachment.then_some(attachment),
        native_turn_id: optional(row, 10)?,
        native_entry_id: optional(row, 11)?,
        service_admitted: row.get(12)?,
        updated_at_ms: row.get(14)?,
    })
}

fn optional<T: std::str::FromStr>(row: &Row<'_>, index: usize) -> Result<Option<T>, StoreError> {
    let value: Option<String> = row.get(index)?;
    value.as_deref().map(parse).transpose()
}
