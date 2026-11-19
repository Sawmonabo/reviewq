use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_domain::identity::NativeTurnId;

use super::{Codex, history::Turn, session::Thread};

const MAX_LOOKUPS: usize = 64;
const KEEP_PROGRESS: Duration = Duration::from_secs(10 * 60);

type Key = (String, NativeTurnId);

#[derive(Default)]
pub(super) struct TurnLookups(Mutex<HashMap<Key, Position>>);

struct Position {
    generation: String,
    cursor: String,
    at: Instant,
}

impl Codex {
    pub(super) async fn original_turn(
        &self,
        thread: &Thread,
        id: &NativeTurnId,
    ) -> Result<Option<Turn>, AppError> {
        let generation = self.history_generation(thread).await?;
        let key = (thread.id.clone(), id.clone());
        let cursor = self.turn_lookups.cursor(&key, &generation)?;
        let page = match self
            .turns(&thread.id, cursor.as_deref(), false, 64, false)
            .await
        {
            Ok(page) => page,
            Err(AppError::NotFound | AppError::CursorExpired) if cursor.is_some() => {
                self.turn_lookups.remember(key, generation, None)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let found = page.data.into_iter().find(|turn| turn.id == id.as_str());
        let next = if found.is_none() {
            page.next_cursor
                .filter(|next| Some(next) != cursor.as_ref())
        } else {
            None
        };
        self.turn_lookups.remember(key, generation, next)?;
        Ok(found)
    }
}

impl TurnLookups {
    fn cursor(&self, key: &Key, generation: &str) -> Result<Option<String>, AppError> {
        let mut entries = self.0.lock().map_err(|_| AppError::Internal)?;
        entries.retain(|_, value| value.at.elapsed() <= KEEP_PROGRESS);
        Ok(entries
            .get(key)
            .filter(|value| value.generation == generation)
            .map(|value| value.cursor.clone()))
    }

    fn remember(
        &self,
        key: Key,
        generation: String,
        cursor: Option<String>,
    ) -> Result<(), AppError> {
        let mut entries = self.0.lock().map_err(|_| AppError::Internal)?;
        if let Some(cursor) = cursor {
            if entries.len() >= MAX_LOOKUPS
                && !entries.contains_key(&key)
                && let Some(oldest) = entries
                    .iter()
                    .min_by_key(|(_, value)| value.at)
                    .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
            entries.insert(
                key,
                Position {
                    generation,
                    cursor,
                    at: Instant::now(),
                },
            );
        } else {
            entries.remove(&key);
        }
        Ok(())
    }
}
