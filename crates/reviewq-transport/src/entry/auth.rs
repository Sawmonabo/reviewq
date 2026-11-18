use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{Actor, AppError};
use subtle::ConstantTimeEq;

use super::control::{ControlRequest, MAX_CONTROL_AGE, ServiceEndpoint, decode, random, value};

const MAX_NONCES: usize = 1024;
const MAX_TICKETS: usize = 128;
const MAX_SESSIONS: usize = 128;
const TICKET_AGE: Duration = Duration::from_secs(60);
const SESSION_AGE: Duration = Duration::from_secs(12 * 60 * 60);

pub(super) struct Auth {
    pub(super) endpoint: ServiceEndpoint,
    pub(super) origin: String,
    browser_host: String,
    native_host: String,
    cookie: String,
    state: Mutex<AuthState>,
}

#[derive(Default)]
struct AuthState {
    nonces: HashMap<[u8; 32], Instant>,
    tickets: HashMap<[u8; 32], Instant>,
    sessions: HashMap<[u8; 32], Session>,
}

struct Session {
    csrf: [u8; 32],
    expires: Instant,
}

impl Auth {
    pub(super) fn new(endpoint: ServiceEndpoint) -> Self {
        let browser_host = endpoint.browser().to_string();
        let native_host = endpoint.native().to_string();
        let cookie = format!("reviewq_{}", endpoint.instance());
        Self {
            origin: format!("http://{browser_host}"),
            browser_host,
            native_host,
            endpoint,
            cookie,
            state: Mutex::default(),
        }
    }

    pub(super) fn allowed_host(&self, headers: &HeaderMap) -> bool {
        let host = value(headers, "host");
        (host == Some(&self.browser_host) || host == Some(&self.native_host))
            && headers.get_all(header::HOST).iter().count() == 1
            && headers.get_all(header::ORIGIN).iter().count() <= 1
            && (!headers.contains_key(header::ORIGIN)
                || value(headers, "origin") == Some(&self.origin))
    }

    pub(super) fn check_origin(&self, headers: &HeaderMap) -> Result<(), AppError> {
        (value(headers, "origin") == Some(&self.origin))
            .then_some(())
            .ok_or(AppError::Forbidden)
    }

    pub(super) fn control<'a>(
        &self,
        method: &'a str,
        url: &'a str,
        headers: &HeaderMap,
        bytes: &[u8],
    ) -> Result<ControlRequest<'a>, AppError> {
        let request = self.endpoint.verify_request(method, url, headers, bytes)?;
        let mut state = self.state.lock().map_err(|_| AppError::Internal)?;
        let now = Instant::now();
        state.nonces.retain(|_, deadline| *deadline > now);
        if state.nonces.contains_key(&request.nonce()) {
            return Err(AppError::Unauthorized);
        }
        if state.nonces.len() >= MAX_NONCES {
            return Err(AppError::Busy);
        }
        state.nonces.insert(
            request.nonce(),
            now + Duration::from_secs(MAX_CONTROL_AGE * 2 + 1),
        );
        Ok(request)
    }

    pub(super) fn launch(&self) -> Result<String, AppError> {
        let mut state = self.state.lock().map_err(|_| AppError::Internal)?;
        let now = Instant::now();
        state.tickets.retain(|_, expiry| *expiry > now);
        if state.tickets.len() >= MAX_TICKETS {
            return Err(AppError::Busy);
        }
        let ticket = random()?;
        if state.tickets.contains_key(&ticket) {
            return Err(AppError::Internal);
        }
        state.tickets.insert(ticket, now + TICKET_AGE);
        Ok(URL_SAFE_NO_PAD.encode(ticket))
    }

    pub(super) fn redeem(&self, ticket: &str) -> Result<(String, HeaderValue), AppError> {
        let ticket = decode(ticket).ok_or(AppError::Unauthorized)?;
        let mut state = self.state.lock().map_err(|_| AppError::Internal)?;
        let now = Instant::now();
        if state
            .tickets
            .get(&ticket)
            .is_none_or(|expiry| *expiry <= now)
        {
            return Err(AppError::Unauthorized);
        }
        state.sessions.retain(|_, session| session.expires > now);
        if state.sessions.len() >= MAX_SESSIONS {
            return Err(AppError::Busy);
        }
        let session = random()?;
        let csrf = random()?;
        if state.sessions.contains_key(&session) {
            return Err(AppError::Internal);
        }
        let cookie = format!(
            "{}={}; Path=/api/v1; HttpOnly; SameSite=Strict",
            self.cookie,
            URL_SAFE_NO_PAD.encode(session)
        );
        let mut cookie = HeaderValue::from_str(&cookie).map_err(|_| AppError::Internal)?;
        cookie.set_sensitive(true);
        state.sessions.insert(
            session,
            Session {
                csrf,
                expires: now + SESSION_AGE,
            },
        );
        state.tickets.remove(&ticket);
        Ok((URL_SAFE_NO_PAD.encode(csrf), cookie))
    }

    pub(super) fn browser_actor(
        &self,
        headers: &HeaderMap,
        mutation: bool,
    ) -> Result<Actor, AppError> {
        let csrf = self.csrf(headers)?;
        if mutation {
            self.check_origin(headers)?;
            let supplied = value(headers, "x-reviewq-csrf")
                .and_then(decode)
                .ok_or(AppError::Forbidden)?;
            if !bool::from(csrf.ct_eq(&supplied)) {
                return Err(AppError::Forbidden);
            }
        }
        Ok(Actor::local_operator())
    }

    pub(super) fn csrf(&self, headers: &HeaderMap) -> Result<[u8; 32], AppError> {
        let cookie = value(headers, "cookie").ok_or(AppError::Unauthorized)?;
        let mut candidates = cookie.split(';').filter_map(|entry| {
            let (key, value) = entry.trim().split_once('=')?;
            (key == self.cookie).then_some(value)
        });
        let token = candidates
            .next()
            .and_then(decode)
            .ok_or(AppError::Unauthorized)?;
        if candidates.next().is_some() {
            return Err(AppError::Unauthorized);
        }
        let state = self.state.lock().map_err(|_| AppError::Internal)?;
        state
            .sessions
            .get(&token)
            .filter(|session| session.expires > Instant::now())
            .map(|session| session.csrf)
            .ok_or(AppError::Unauthorized)
    }
}
