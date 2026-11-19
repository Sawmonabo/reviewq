use reviewq_app::access::AppError;
use reviewq_app::queue::{CompactTicket, UnmatchedTicket};
use rusqlite::{Connection, params};

use crate::StoreError;

use super::{parse, text};

const UNMATCHED: &str = "SELECT i.ticket_key,min(i.board_id) AS board_id FROM scoped_issues i
     WHERE NOT EXISTS (
         SELECT 1 FROM scoped_review_issues m WHERE m.ticket_key=i.ticket_key)
     GROUP BY i.ticket_key";

pub(super) fn unmatched_total(db: &Connection) -> Result<u64, StoreError> {
    let query = format!("SELECT count(*) FROM ({UNMATCHED})");
    let total = db
        .prepare_cached(&query)?
        .query_row([], |row| row.get::<_, i64>(0))?;
    u64::try_from(total).map_err(|_| AppError::Internal.into())
}

pub(super) fn unmatched(
    db: &Connection,
    after: Option<&str>,
    limit: u16,
) -> Result<Vec<UnmatchedTicket>, StoreError> {
    let query = format!(
        "SELECT i.ticket_key,i.title,i.status,i.assignee,i.priority
         FROM ({UNMATCHED}) u JOIN scoped_issues i USING(ticket_key,board_id)
         WHERE i.ticket_key>?1 ORDER BY i.ticket_key LIMIT ?2"
    );
    let mut select = db.prepare_cached(&query)?;
    let mut rows = select.query(params![after.unwrap_or(""), limit])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        items.push(UnmatchedTicket {
            ticket: CompactTicket {
                key: parse(text(row, 0)?)?,
                title: row.get(1)?,
            },
            status: row.get(2)?,
            assignee: row.get(3)?,
            priority: row.get(4)?,
        });
    }
    Ok(items)
}
