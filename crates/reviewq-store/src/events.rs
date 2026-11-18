//! Durable scoped invalidations and same-snapshot projection helpers.

use std::time::{SystemTime, UNIX_EPOCH};

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::events::{
    EventLog, Invalidation, MAX_REPLAY_LIMIT, Replay, ResetReason, ResourceKey, Snapshot,
};
use reviewq_domain::identity::EventCursor;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::connection::{WriteTx, read_head};
use crate::{SqliteStore, StoreError};

const RETAINED_EVENTS: i64 = 4096;
const FORMAT: u8 = 1;

#[derive(Serialize, Deserialize)]
struct StoredInvalidation {
    format: u8,
    keys: Vec<StoredKey>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
enum StoredKey {
    Queue,
    Review(Option<String>),
    Run(Option<String>),
    Session(Option<String>),
    History(Option<String>),
    Changes(Option<String>),
    Discussions(Option<String>),
    Report(Option<String>),
    Action(Option<String>),
    Followup(Option<String>),
    Ticket(Option<String>),
    Notification(Option<String>),
    Settings,
}

impl From<&ResourceKey> for StoredKey {
    fn from(value: &ResourceKey) -> Self {
        match value {
            ResourceKey::Queue => Self::Queue,
            ResourceKey::Review(id) => Self::Review(id.as_ref().map(ToString::to_string)),
            ResourceKey::Run(id) => Self::Run(id.as_ref().map(ToString::to_string)),
            ResourceKey::Session(id) => Self::Session(id.as_ref().map(ToString::to_string)),
            ResourceKey::History(id) => Self::History(id.as_ref().map(ToString::to_string)),
            ResourceKey::Changes(id) => Self::Changes(id.as_ref().map(ToString::to_string)),
            ResourceKey::Discussions(id) => Self::Discussions(id.as_ref().map(ToString::to_string)),
            ResourceKey::Report(id) => Self::Report(id.as_ref().map(ToString::to_string)),
            ResourceKey::Action(id) => Self::Action(id.as_ref().map(ToString::to_string)),
            ResourceKey::Followup(id) => Self::Followup(id.as_ref().map(ToString::to_string)),
            ResourceKey::Ticket(id) => Self::Ticket(id.as_ref().map(ToString::to_string)),
            ResourceKey::Notification(id) => {
                Self::Notification(id.as_ref().map(ToString::to_string))
            }
            ResourceKey::Settings => Self::Settings,
        }
    }
}

impl TryFrom<StoredKey> for ResourceKey {
    type Error = StoreError;

    fn try_from(value: StoredKey) -> Result<Self, StoreError> {
        fn key<T: std::str::FromStr>(value: Option<String>) -> Result<Option<T>, StoreError> {
            value
                .map(|id| id.parse().map_err(|_| AppError::Internal.into()))
                .transpose()
        }
        Ok(match value {
            StoredKey::Queue => Self::Queue,
            StoredKey::Review(id) => Self::Review(key(id)?),
            StoredKey::Run(id) => Self::Run(key(id)?),
            StoredKey::Session(id) => Self::Session(key(id)?),
            StoredKey::History(id) => Self::History(key(id)?),
            StoredKey::Changes(id) => Self::Changes(key(id)?),
            StoredKey::Discussions(id) => Self::Discussions(key(id)?),
            StoredKey::Report(id) => Self::Report(key(id)?),
            StoredKey::Action(id) => Self::Action(key(id)?),
            StoredKey::Followup(id) => Self::Followup(key(id)?),
            StoredKey::Ticket(id) => Self::Ticket(key(id)?),
            StoredKey::Notification(id) => Self::Notification(key(id)?),
            StoredKey::Settings => Self::Settings,
        })
    }
}

impl WriteTx<'_> {
    /// Records a versioned, bounded journal entry in the mutation transaction.
    pub(crate) fn emit(&mut self, keys: &[ResourceKey]) -> Result<EventCursor, StoreError> {
        let invalidation = Invalidation::new(keys.to_vec())?;
        let entry = StoredInvalidation {
            format: FORMAT,
            keys: invalidation.keys().iter().map(StoredKey::from).collect(),
        };
        let resources = serde_json::to_string(&entry).map_err(|_| AppError::Internal)?;
        let cursor = read_head(&self.sql)?
            .checked_add(1)
            .ok_or(AppError::Internal)?;
        let head = cursor.as_sqlite_integer();
        let floor = (head - RETAINED_EVENTS).max(0);
        self.sql.execute(
            "INSERT INTO events(cursor, resources, committed_at_ms) VALUES(?1, ?2, ?3)",
            params![head, resources, now_ms()?],
        )?;
        self.sql
            .execute("DELETE FROM events WHERE cursor <= ?1", [floor])?;
        self.sql.execute(
            "UPDATE service_state SET event_head = ?1, event_floor = ?2 WHERE singleton = 1",
            params![head, floor],
        )?;
        self.wakeup = Some(cursor);
        Ok(cursor)
    }
}

impl EventLog for SqliteStore {
    fn replay(&self, after: EventCursor, limit: u16) -> AppFuture<'_, Replay> {
        Box::pin(async move {
            if !(1..=MAX_REPLAY_LIMIT).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |tx| replay(tx, after, limit))
                .await
                .map_err(StoreError::app)
        })
    }
}

fn replay(connection: &Connection, after: EventCursor, limit: u16) -> Result<Replay, StoreError> {
    let (floor, head) = connection.query_row(
        "SELECT event_floor, event_head FROM service_state WHERE singleton = 1",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let floor = EventCursor::try_from(floor).map_err(|_| AppError::Internal)?;
    let head = EventCursor::try_from(head).map_err(|_| AppError::Internal)?;
    let reason = if after < floor {
        Some(ResetReason::CursorExpired)
    } else if after > head {
        Some(ResetReason::CursorAhead)
    } else {
        None
    };
    if let Some(reason) = reason {
        return Ok(Replay::Reset {
            floor,
            head,
            reason,
        });
    }

    let mut statement = connection.prepare_cached(
        "SELECT cursor, resources FROM events WHERE cursor > ?1 ORDER BY cursor LIMIT ?2",
    )?;
    let mut rows = statement.query(params![after.as_sqlite_integer(), limit])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        let cursor =
            EventCursor::try_from(row.get::<_, i64>(0)?).map_err(|_| AppError::Internal)?;
        let resources = row.get_ref(1)?.as_str().map_err(|_| AppError::Internal)?;
        let event: StoredInvalidation =
            serde_json::from_str(resources).map_err(|_| AppError::Internal)?;
        if event.format != FORMAT {
            return Err(AppError::Internal.into());
        }
        let keys: Vec<_> = event
            .keys
            .into_iter()
            .map(ResourceKey::try_from)
            .collect::<Result<_, _>>()?;
        let invalidation = Invalidation::new(keys).map_err(|_| AppError::Internal)?;
        events.push((cursor, invalidation));
    }
    Ok(Replay::Batch {
        floor,
        head,
        events,
    })
}

pub(crate) fn snapshot<T>(connection: &Connection, value: T) -> Result<Snapshot<T>, StoreError> {
    Ok(Snapshot {
        value,
        cursor: read_head(connection)?,
    })
}

pub(crate) fn now_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::Internal)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| AppError::Internal.into())
}
