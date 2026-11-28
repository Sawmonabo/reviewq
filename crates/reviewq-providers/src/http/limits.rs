//! Reads the retry and rate-limit headers GitLab and Jira Cloud send, for every caller.
//!
//! GitLab sends `RateLimit-Remaining` and `RateLimit-Reset` (Unix seconds) on every answer,
//! and `Retry-After` (seconds) on a 429. Jira Cloud sends `X-RateLimit-Remaining`, and on a
//! 429 `Retry-After` (seconds) and `X-RateLimit-Reset` (an ISO 8601 time).
//! [GitLab](https://docs.gitlab.com/administration/settings/user_and_ip_rate_limits/) ·
//! [Jira Cloud](https://developer.atlassian.com/cloud/jira/platform/rate-limiting/)

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{self, HeaderMap};

/// The longest wait ReviewQ takes on `Retry-After`: GitLab's longest, for its daily limits.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// The longest wait ReviewQ takes on a reset time: GitLab's and Jira's windows are a minute
/// or an hour, so a later reset is a skewed clock or a nonsense value.
const MAX_RESET: Duration = Duration::from_secs(60 * 60);
/// An integer reset at or past this many seconds is a Unix time, as GitLab sends; a smaller
/// one is a count of seconds, as earlier IETF `RateLimit` drafts defined it.
const UNIX_TIME_FLOOR: u64 = 1_000_000_000;
const REMAINING: [&str; 2] = ["ratelimit-remaining", "x-ratelimit-remaining"];
const RESET: [&str; 2] = ["ratelimit-reset", "x-ratelimit-reset"];

/// The wait `Retry-After` asks for, in seconds or as an HTTP date. `None` when it is absent,
/// zero, already past or unreadable.
pub(super) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let text = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    let wait = match text.parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => until(httpdate::parse_http_date(text).ok()?, headers)?,
    };
    (!wait.is_zero()).then(|| wait.min(MAX_RETRY_AFTER))
}

/// The wait until the service's rate-limit window resets. Read only for a rate-limit answer:
/// GitLab sends its reset on every answer, whatever the status.
pub(super) fn reset(headers: &HeaderMap) -> Option<Duration> {
    let text = RESET
        .iter()
        .find_map(|name| headers.get(*name))?
        .to_str()
        .ok()?
        .trim();
    let wait = match text.parse::<u64>() {
        Ok(time) if time >= UNIX_TIME_FLOOR => {
            until(UNIX_EPOCH.checked_add(Duration::from_secs(time))?, headers)?
        }
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => {
            let seconds = text.parse::<jiff::Timestamp>().ok()?.as_second();
            let time = UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(seconds).ok()?))?;
            until(time, headers)?
        }
    };
    (!wait.is_zero()).then(|| wait.min(MAX_RESET))
}

/// The wait a successful answer asks for when it spent the window's last request: until the
/// reset it names. `None` while requests remain or when it names no reset.
pub(super) fn spent(headers: &HeaderMap) -> Option<Duration> {
    let remaining = REMAINING
        .iter()
        .find_map(|name| headers.get(*name))?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    (remaining == 0).then(|| reset(headers)).flatten()
}

/// The time from the answer's own clock to `time`, so a skewed local clock does not stretch
/// or cut the wait: the answer's `Date`, else this machine's clock. `None` when already past.
fn until(time: SystemTime, headers: &HeaderMap) -> Option<Duration> {
    let now = headers
        .get(header::DATE)
        .and_then(|value| httpdate::parse_http_date(value.to_str().ok()?).ok())
        .unwrap_or_else(SystemTime::now);
    time.duration_since(now).ok()
}
