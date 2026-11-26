//! Whether a Claude companion is live, gone or unknown. A companion reports every 5 seconds
//! and is stored with the wall-clock time of its last report, or 0 once it is stored as ended.
//!
//! - Live: it reported within the last 30 seconds and not more than 5 seconds ahead of the
//!   wall clock. Live needs no watch.
//! - Gone: it is stored as ended, or ReviewQ has watched for 35 seconds without a break and
//!   it is not live.
//! - Unknown: anything else. A stopped service, a sleeping machine and a clock step all look
//!   like silence from a companion that is still running, so silence counts only once the
//!   watch has run 35 seconds unbroken, and unknown lasts at most that long.
//!
//! Once the watch has run that long, live and gone are exact opposites. A reader that acts
//! on presence asks `reviewq_companion_live(seen_at_ms)`, one that acts on absence asks
//! `reviewq_companion_gone(seen_at_ms)`, and a display shows the stored state while
//! `NOT reviewq_companion_gone(seen_at_ms)`. Each answers NULL for a missing report time.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;

/// How long a companion may stay silent before it counts as gone.
pub(crate) const SILENCE: Duration = Duration::from_secs(30);
/// How long ReviewQ watches without a break before silence counts: the 30 seconds a companion
/// may stay silent, and one more of its 5-second reports.
const WATCH_BEFORE_SILENCE_COUNTS: Duration = Duration::from_secs(35);
/// How far the wall clock may move apart from elapsed time between two reads. More means the
/// machine slept or the clock was set, and stored report times say nothing yet.
const CLOCK_TOLERANCE: Duration = Duration::from_secs(5);

/// Since when ReviewQ has watched companions without a break. One per open store.
pub(crate) struct CompanionWatch(Mutex<Watch>);

struct Watch {
    since: Instant,
    read: (Instant, SystemTime),
}

impl CompanionWatch {
    /// A watch that begins now, as ReviewQ opens its store.
    pub(crate) fn start() -> Self {
        let now = Instant::now();
        Self(Mutex::new(Watch {
            since: now,
            read: (now, SystemTime::now()),
        }))
    }

    /// The wall-clock time in Unix milliseconds, and whether the watch has run long enough
    /// for silence to count. Starts the watch again when the wall clock and elapsed time moved
    /// apart since the last read.
    fn now(&self) -> (i64, bool) {
        let now = (Instant::now(), SystemTime::now());
        let mut watch = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let elapsed = now.0.duration_since(watch.read.0);
        let apart = match now.1.duration_since(watch.read.1) {
            Ok(wall) => wall.abs_diff(elapsed),
            Err(backwards) => backwards.duration() + elapsed,
        };
        if apart > CLOCK_TOLERANCE {
            watch.since = now.0;
        }
        watch.read = now;
        let established = now.0.duration_since(watch.since) >= WATCH_BEFORE_SILENCE_COUNTS;
        let wall = now.1.duration_since(UNIX_EPOCH).unwrap_or_default();
        (millis(wall), established)
    }

    /// Whether a companion that last reported at `seen` is live.
    fn live(&self, seen: i64) -> bool {
        live(seen, self.now().0)
    }

    /// Whether a companion that last reported at `seen` is gone.
    fn gone(&self, seen: i64) -> bool {
        let (wall, established) = self.now();
        seen == 0 || (established && !live(seen, wall))
    }

    /// Lets silence count from now on, as if ReviewQ had already watched long enough.
    #[cfg(test)]
    pub(crate) fn watched_long_enough(&self) {
        let mut watch = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        watch.since = now
            .checked_sub(WATCH_BEFORE_SILENCE_COUNTS)
            .unwrap_or(watch.since);
        watch.read = (now, SystemTime::now());
    }
}

/// An SQL condition: the companion whose report time is the expression `seen` is unknown.
/// A reader that would act on absence waits while it holds.
pub(crate) fn unknown(seen: &str) -> String {
    format!("(NOT reviewq_companion_live({seen}) AND NOT reviewq_companion_gone({seen}))")
}

/// Whether a report at `seen` is live at wall-clock time `wall`.
fn live(seen: i64, wall: i64) -> bool {
    seen > wall.saturating_sub(millis(SILENCE))
        && seen <= wall.saturating_add(millis(CLOCK_TOLERANCE))
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Registers `reviewq_companion_live(seen_at_ms)` and `reviewq_companion_gone(seen_at_ms)` on
/// a connection of the store that owns `watch`.
pub(crate) fn register(db: &Connection, watch: &Arc<CompanionWatch>) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS;
    let live = Arc::clone(watch);
    db.create_scalar_function("reviewq_companion_live", 1, flags, move |context| {
        Ok(context.get::<Option<i64>>(0)?.map(|seen| live.live(seen)))
    })?;
    let gone = Arc::clone(watch);
    db.create_scalar_function("reviewq_companion_gone", 1, flags, move |context| {
        Ok(context.get::<Option<i64>>(0)?.map(|seen| gone.gone(seen)))
    })
}
