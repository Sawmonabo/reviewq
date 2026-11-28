//! What a connection learns about the daemon's socket: whether one was reached and how long
//! it has been bound, on a clock that a change of the wall clock cannot move forward.

use std::time::{Duration, SystemTime};

use tokio::time::Instant;

/// What the latest attempt to connect learned about the profile's Codex daemon.
#[derive(Clone, Copy, Default)]
pub(in crate::codex) enum Reach {
    /// No daemon was reached. That alone does not show that none runs.
    #[default]
    Unknown,
    /// A daemon accepted the connection on a socket bound this long ago, when its bind time is
    /// readable.
    Bound(Option<BoundAge>),
}

/// How long a daemon's socket has been bound: credited once when this connection first
/// reaches that socket, and counted on the monotonic clock from then on.
#[derive(Clone, Copy)]
pub(in crate::codex) struct BoundAge {
    credit: Duration,
    sighted: Instant,
}

impl BoundAge {
    pub(in crate::codex) fn elapsed(&self) -> Duration {
        self.credit + self.sighted.elapsed()
    }
}

/// The ages credited to the sockets this connection reached. The first credit is the smaller
/// of the wall clock's age of the socket and its age on a clock that reads the wall clock
/// once, when the connection was made, and runs on the monotonic clock after. So a clock set
/// forward or back, or a wake from sleep, after the connection was made cannot make a socket
/// look older than it is. A later sighting of the same socket keeps its credit, so a
/// reconnect neither restarts nor shortens the count.
pub(super) struct Sightings {
    wall: SystemTime,
    steady: Instant,
    last: Option<(SystemTime, BoundAge)>,
}

impl Sightings {
    pub(super) fn new() -> Self {
        Self {
            wall: SystemTime::now(),
            steady: Instant::now(),
            last: None,
        }
    }

    pub(super) fn age(&mut self, bound: SystemTime) -> BoundAge {
        if let Some((seen, age)) = self.last
            && seen == bound
        {
            return age;
        }
        let since = |now: SystemTime| now.duration_since(bound).unwrap_or_default();
        let age = BoundAge {
            credit: since(SystemTime::now()).min(since(self.wall + self.steady.elapsed())),
            sighted: Instant::now(),
        };
        self.last = Some((bound, age));
        age
    }
}
