use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, StatusCode, Url, header};
use reviewq_app::access::AppError;
use reviewq_app::settings::SourceLease;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, sleep_until, timeout_at};

mod buffer;
pub(crate) use buffer::{ResponseReader, private_file};

const DEADLINE: Duration = Duration::from_secs(30);
const MAX_RETRIES: u8 = 2;
const MAX_ORIGINS: usize = 64;
const BODY_LIMIT: usize = 32 * 1024 * 1024;

/// Common concurrency, cooldown and connection pooling for provider reads and settings probes.
pub struct SourceNetwork {
    client: Client,
    loopback: Client,
    permits: Arc<Semaphore>,
    cooldowns: Mutex<HashMap<String, Instant>>,
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
        url: Url,
        headers: header::HeaderMap,
        max: usize,
        lease: Option<&SourceLease>,
    ) -> Result<(header::HeaderMap, Vec<u8>), AppError> {
        let mut response = self.get(url, headers, lease).await?;
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

    pub(crate) async fn get(
        &self,
        url: Url,
        headers: header::HeaderMap,
        lease: Option<&SourceLease>,
    ) -> Result<Response, AppError> {
        let origin = url.origin().ascii_serialization();
        let local = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        let client = if local { &self.loopback } else { &self.client };
        let deadline = Instant::now() + DEADLINE;
        for retry in 0..=MAX_RETRIES {
            let permit = self.permit(&origin, deadline).await?;
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
                    match status {
                        StatusCode::OK => {
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
                    let transient = status == StatusCode::TOO_MANY_REQUESTS
                        || status == StatusCode::REQUEST_TIMEOUT
                        || status.is_server_error();
                    let failure = if status == StatusCode::TOO_MANY_REQUESTS {
                        AppError::Busy
                    } else {
                        AppError::Unavailable
                    };
                    if transient {
                        let pause =
                            retry_after(response.headers()).unwrap_or_else(|| jitter(retry));
                        self.cooldown(&origin, pause)?;
                    }
                    drop(response);
                    drop(permit);
                    if !transient || retry == MAX_RETRIES {
                        return Err(failure);
                    }
                }
                Err(_) => {
                    drop(permit);
                    if retry == MAX_RETRIES {
                        return Err(AppError::Unavailable);
                    }
                    self.cooldown(&origin, jitter(retry))?;
                }
            }
        }
        Err(AppError::Unavailable)
    }

    async fn permit(
        &self,
        origin: &str,
        deadline: Instant,
    ) -> Result<OwnedSemaphorePermit, AppError> {
        loop {
            if let Some(until) = self.cooldown_until(origin)?
                && until > Instant::now()
            {
                if until >= deadline {
                    return Err(AppError::Busy);
                }
                sleep_until(until).await;
            }
            let permit = timeout_at(deadline, self.permits.clone().acquire_owned())
                .await
                .map_err(|_| AppError::Busy)?
                .map_err(|_| AppError::Unavailable)?;
            if self
                .cooldown_until(origin)?
                .is_none_or(|until| until <= Instant::now())
            {
                return Ok(permit);
            }
        }
    }

    fn cooldown_until(&self, origin: &str) -> Result<Option<Instant>, AppError> {
        Ok(self
            .cooldowns
            .lock()
            .map_err(|_| AppError::Internal)?
            .get(origin)
            .copied())
    }

    fn cooldown(&self, origin: &str, pause: Duration) -> Result<(), AppError> {
        let now = Instant::now();
        let until = now.checked_add(pause).ok_or(AppError::Busy)?;
        let mut entries = self.cooldowns.lock().map_err(|_| AppError::Internal)?;
        entries.retain(|_, prior| *prior > now);
        if entries.len() >= MAX_ORIGINS && !entries.contains_key(origin) {
            return Err(AppError::Busy);
        }
        let prior = entries.entry(origin.into()).or_insert(until);
        *prior = (*prior).max(until);
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
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never());
    if loopback {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|_| AppError::Internal)
}

fn retry_after(headers: &header::HeaderMap) -> Option<Duration> {
    if let Some(text) = headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
    {
        return text
            .parse::<u64>()
            .ok()
            .map(Duration::from_secs)
            .or_else(|| {
                httpdate::parse_http_date(text)
                    .ok()
                    .map(|time| time.duration_since(SystemTime::now()).unwrap_or_default())
            });
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let reset: u64 = headers
        .get("ratelimit-reset")?
        .to_str()
        .ok()?
        .parse()
        .ok()?;
    Some(Duration::from_secs(reset.saturating_sub(now)))
}

fn jitter(retry: u8) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos());
    Duration::from_millis((100 << retry) + u64::from(nanos % 103))
}
