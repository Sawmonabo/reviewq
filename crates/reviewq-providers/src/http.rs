use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, StatusCode, Url, header};
use reviewq_app::access::{AppError, Service, SourceProblem};
use reviewq_app::settings::SourceLease;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, sleep_until, timeout_at};

mod buffer;
mod limits;
mod stream;
pub(crate) use buffer::{ResponseReader, private_file};

const DEADLINE: Duration = Duration::from_secs(30);
const MAX_RETRIES: u8 = 2;
const MAX_ORIGINS: usize = 64;
const BODY_LIMIT: usize = 32 * 1024 * 1024;
/// The name ReviewQ gives every outside service, so the service can tell its requests apart.
const USER_AGENT: &str = concat!("ReviewQ/", env!("CARGO_PKG_VERSION"));
/// Atlassian's API gateway, which a scoped Jira token reads Jira through instead of the site.
pub(crate) const ATLASSIAN_GATEWAY: &str = "api.atlassian.com";

/// Common concurrency, cooldown and connection pooling for provider reads and settings probes.
pub struct SourceNetwork {
    client: Client,
    loopback: Client,
    permits: Arc<Semaphore>,
    cooldowns: Mutex<HashMap<String, Cooldown>>,
}

/// When an origin may next be called, and whether the service itself asked for the wait.
#[derive(Clone, Copy)]
struct Cooldown {
    until: Instant,
    asked: bool,
}

impl SourceNetwork {
    pub fn new() -> Result<Self, AppError> {
        Ok(Self {
            client: client(false)?,
            loopback: client(true)?,
            permits: Arc::new(Semaphore::new(4)),
            cooldowns: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) async fn bounded_response(
        &self,
        service: Service,
        url: Url,
        headers: header::HeaderMap,
        max: usize,
        lease: Option<&SourceLease>,
    ) -> Result<(header::HeaderMap, Vec<u8>), AppError> {
        let mut response = self.get(service, url, headers, lease).await?;
        let max = max.min(BODY_LIMIT);
        if response
            .inner
            .content_length()
            .is_some_and(|length| length > max as u64)
        {
            return Err(AppError::PayloadTooLarge);
        }
        let metadata = response.headers().clone();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > max - bytes.len() {
                return Err(AppError::PayloadTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        if let Some(lease) = lease {
            lease.check().await?;
        }
        Ok((metadata, bytes))
    }

    /// Sends one GET to `service`, retrying a transient failure within the call's deadline.
    ///
    /// Before each try it waits out the origin's pause, or fails at once when the pause
    /// outlasts the deadline: `SourceProblem::Wait` when the service asked for it. A 429 waits
    /// for `Retry-After`, else the rate-limit reset, else a jittered backoff; a 408 or 5xx
    /// waits for `Retry-After`, else the backoff. A 200 that spends the window's last request
    /// pauses the origin until the reset. Any 401 and a JSON 403 fail as `Forbidden`; other
    /// answers that are not the API's fail as [`not_api`] names them, with a web page from
    /// Atlassian's gateway, which the saved site did not send, as `SourceProblem::GatewayPage`.
    pub(crate) async fn get(
        &self,
        service: Service,
        url: Url,
        headers: header::HeaderMap,
        lease: Option<&SourceLease>,
    ) -> Result<Response, AppError> {
        let origin = url.origin().ascii_serialization();
        let gateway = service == Service::Jira && url.host_str() == Some(ATLASSIAN_GATEWAY);
        let local = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        let client = if local { &self.loopback } else { &self.client };
        let deadline = Instant::now() + DEADLINE;
        for retry in 0..=MAX_RETRIES {
            let permit = self.permit(service, &origin, deadline).await?;
            if let Some(lease) = lease {
                timeout_at(deadline, lease.check())
                    .await
                    .map_err(|_| AppError::Busy)??;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AppError::Unavailable);
            }
            match client
                .get(url.clone())
                .headers(headers.clone())
                .timeout(remaining)
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    if let Some(problem) = not_api(service, status, response.headers()) {
                        return Err(AppError::Source(match problem {
                            SourceProblem::WrongAddress(_) if gateway => SourceProblem::GatewayPage,
                            problem => problem,
                        }));
                    }
                    match status {
                        StatusCode::OK => {
                            if let Some(wait) = limits::spent(response.headers()) {
                                // A full cooldown table loses only this early pause.
                                let _ = self.cooldown(&origin, wait, true);
                            }
                            return Ok(Response {
                                inner: response,
                                _permit: permit,
                                seen: 0,
                            });
                        }
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                            return Err(AppError::Forbidden);
                        }
                        StatusCode::NOT_FOUND => return Err(AppError::NotFound),
                        _ => {}
                    }
                    let limited = status == StatusCode::TOO_MANY_REQUESTS;
                    let transient = limited
                        || status == StatusCode::REQUEST_TIMEOUT
                        || status.is_server_error();
                    if transient {
                        let asked = limits::retry_after(response.headers()).or_else(|| {
                            limited.then(|| limits::reset(response.headers())).flatten()
                        });
                        self.cooldown(
                            &origin,
                            asked.unwrap_or_else(|| backoff(retry, limited)),
                            asked.is_some() || limited,
                        )?;
                    }
                    drop(response);
                    drop(permit);
                    if !transient || retry == MAX_RETRIES {
                        return Err(if limited {
                            self.wait(service, &origin)
                        } else {
                            AppError::Unavailable
                        });
                    }
                }
                Err(_) => {
                    drop(permit);
                    if retry == MAX_RETRIES {
                        return Err(AppError::Unavailable);
                    }
                    self.cooldown(&origin, backoff(retry, false), false)?;
                }
            }
        }
        Err(AppError::Unavailable)
    }

    /// How long the service at `origin` still asks ReviewQ to wait, or zero.
    pub(crate) fn paused_for(&self, origin: &Url) -> Duration {
        self.asked_pause(&origin.origin().ascii_serialization())
    }

    /// The failure for a call `service` asked to wait, carrying the wait left at `origin`.
    fn wait(&self, service: Service, origin: &str) -> AppError {
        let left = self.asked_pause(origin);
        AppError::Source(SourceProblem::Wait(
            service,
            (!left.is_zero()).then_some(left),
        ))
    }

    fn asked_pause(&self, origin: &str) -> Duration {
        self.cooldown_until(origin)
            .ok()
            .flatten()
            .filter(|cooldown| cooldown.asked)
            .map_or(Duration::ZERO, |cooldown| {
                cooldown.until.saturating_duration_since(Instant::now())
            })
    }

    async fn permit(
        &self,
        service: Service,
        origin: &str,
        deadline: Instant,
    ) -> Result<OwnedSemaphorePermit, AppError> {
        loop {
            if let Some(Cooldown { until, asked }) = self.cooldown_until(origin)?
                && until > Instant::now()
            {
                if until >= deadline {
                    return Err(if asked {
                        self.wait(service, origin)
                    } else {
                        AppError::Unavailable
                    });
                }
                sleep_until(until).await;
            }
            let permit = timeout_at(deadline, self.permits.clone().acquire_owned())
                .await
                .map_err(|_| AppError::Busy)?
                .map_err(|_| AppError::Unavailable)?;
            if self
                .cooldown_until(origin)?
                .is_none_or(|cooldown| cooldown.until <= Instant::now())
            {
                return Ok(permit);
            }
        }
    }

    fn cooldown_until(&self, origin: &str) -> Result<Option<Cooldown>, AppError> {
        Ok(self
            .cooldowns
            .lock()
            .map_err(|_| AppError::Internal)?
            .get(origin)
            .copied())
    }

    /// Pauses `origin` for `pause`, keeping a longer pause already set.
    fn cooldown(&self, origin: &str, pause: Duration, asked: bool) -> Result<(), AppError> {
        let now = Instant::now();
        let next = Cooldown {
            until: now.checked_add(pause).ok_or(AppError::Busy)?,
            asked,
        };
        let mut entries = self.cooldowns.lock().map_err(|_| AppError::Internal)?;
        entries.retain(|_, prior| prior.until > now);
        if entries.len() >= MAX_ORIGINS && !entries.contains_key(origin) {
            return Err(AppError::Busy);
        }
        let prior = entries.entry(origin.into()).or_insert(next);
        if next.until > prior.until {
            *prior = next;
        }
        Ok(())
    }
}

pub(crate) struct Response {
    inner: reqwest::Response,
    _permit: OwnedSemaphorePermit,
    seen: usize,
}

impl Response {
    pub(crate) fn headers(&self) -> &header::HeaderMap {
        self.inner.headers()
    }

    pub(crate) fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    pub(crate) fn check_body_length(&self) -> Result<(), AppError> {
        if self
            .content_length()
            .is_some_and(|size| size > BODY_LIMIT as u64)
        {
            return Err(AppError::PayloadTooLarge);
        }
        Ok(())
    }

    pub(crate) async fn chunk(
        &mut self,
    ) -> Result<Option<impl AsRef<[u8]> + std::ops::Deref<Target = [u8]>>, AppError> {
        let chunk = self
            .inner
            .chunk()
            .await
            .map_err(|_| AppError::Unavailable)?;
        if let Some(chunk) = &chunk {
            if chunk.len() > BODY_LIMIT - self.seen {
                return Err(AppError::PayloadTooLarge);
            }
            self.seen += chunk.len();
        }
        Ok(chunk)
    }
}

fn client(loopback: bool) -> Result<Client, AppError> {
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(DEADLINE)
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never());
    if loopback {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|_| AppError::Internal)
}

/// What answered when it was not the API, or `None` for an API answer.
///
/// A 401 is refused access whatever it carries: Atlassian answers some with a web page.
/// Cloudflare marks every challenge and block with `cf-mitigated`. A 403 web page refused
/// the request, from the service or from something in front of it. A redirect, which
/// ReviewQ never follows, or a web page on a success or another 4xx answered where the API
/// answers JSON, its errors too. An HTML 408, 429 or 5xx stays a transient fault.
/// [Cloudflare](https://developers.cloudflare.com/cloudflare-challenges/challenge-types/challenge-pages/detect-response/)
fn not_api(
    service: Service,
    status: StatusCode,
    headers: &header::HeaderMap,
) -> Option<SourceProblem> {
    if status == StatusCode::UNAUTHORIZED {
        return None;
    }
    if headers.contains_key("cf-mitigated") {
        return Some(SourceProblem::BotCheck(service));
    }
    let html = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.as_bytes().get(..9))
        .is_some_and(|start| start.eq_ignore_ascii_case(b"text/html"));
    if html && status == StatusCode::FORBIDDEN {
        return Some(SourceProblem::RefusedPage(service));
    }
    let page = status.is_success()
        || (status.is_client_error()
            && !matches!(
                status,
                StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
            ));
    (status.is_redirection() || (html && page)).then_some(SourceProblem::WrongAddress(service))
}

/// The wait before retry number `retry + 1` when the service named none. A rate-limit answer
/// takes Atlassian's backoff: 2 seconds, doubled for each retry, times a factor from 0.7 to
/// 1.3. A connection error, 408 or 5xx is a blip, retried quickly: 100 ms doubled for each
/// retry, plus up to 102 ms of jitter.
/// [Jira Cloud](https://developer.atlassian.com/cloud/jira/platform/rate-limiting/)
fn backoff(retry: u8, limited: bool) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos());
    Duration::from_millis(if limited {
        (2_000 << retry) * (700 + u64::from(nanos % 601)) / 1_000
    } else {
        (100 << retry) + u64::from(nanos % 103)
    })
}

#[cfg(test)]
mod tests;
