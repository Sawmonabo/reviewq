//! Installation-scoped browser grants. Bearer tokens never enter the saved ledger.
use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

use axum::http::{HeaderMap, HeaderValue, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::AppError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::control::{ServiceEndpoint, decode, random, unix_seconds};

const MAX_SESSIONS: usize = 128;
const SESSION_SECONDS: u64 = 12 * 60 * 60;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSessionGrants {
    scope: String,
    expires: HashMap<String, u64>,
}

type Save = dyn Fn(&BrowserSessionGrants) -> Result<(), AppError> + Send + Sync;

pub struct BrowserSessions {
    cookie: String,
    grants: RwLock<BrowserSessionGrants>,
    save: Box<Save>,
    issuance: Mutex<()>,
}

impl BrowserSessions {
    pub fn restore(
        endpoint: &ServiceEndpoint,
        directory_id: (u64, u64),
        database_id: (u64, u64),
        stored: Option<BrowserSessionGrants>,
        save: impl Fn(&BrowserSessionGrants) -> Result<(), AppError> + Send + Sync + 'static,
    ) -> Result<Self, AppError> {
        let identity = endpoint.identity();
        let scope = serde_json::to_vec(&(
            &identity.profile_id,
            &identity.store_id,
            endpoint.browser(),
            directory_id,
            database_id,
        ))
        .map_err(|_| AppError::Internal)?;
        let scope = URL_SAFE_NO_PAD.encode(digest(b"reviewq-browser-scope-v1", &scope));
        let cookie = format!("reviewq_{scope}");
        let mut grants = stored
            .filter(|saved| {
                saved.scope == scope
                    && saved.expires.len() <= MAX_SESSIONS
                    && saved.expires.keys().all(|key| decode(key).is_some())
            })
            .unwrap_or_else(|| BrowserSessionGrants {
                scope,
                expires: HashMap::new(),
            });
        let now = unix_seconds()?;
        grants.expires.retain(|_, expiry| valid(*expiry, now));
        Ok(Self {
            cookie,
            grants: RwLock::new(grants),
            save: Box::new(save),
            issuance: Mutex::new(()),
        })
    }

    fn token(&self, headers: &HeaderMap) -> Option<[u8; 32]> {
        let mut values = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|cookie| cookie.split(';'))
            .filter_map(|item| item.trim().split_once('='))
            .filter_map(|(name, value)| (name == self.cookie).then_some(value));
        let token = decode(values.next()?)?;
        values.next().is_none().then_some(token)
    }

    fn authorized(&self, headers: &HeaderMap, now: u64) -> Result<Option<[u8; 32]>, AppError> {
        let Some(token) = self.token(headers) else {
            return Ok(None);
        };
        let grants = self.grants.read().map_err(|_| AppError::Internal)?;
        Ok(grants
            .expires
            .get(&verifier(&token))
            .copied()
            .filter(|expiry| valid(*expiry, now))
            .map(|_| token))
    }

    pub(super) fn csrf(&self, headers: &HeaderMap) -> Result<[u8; 32], AppError> {
        let token = self
            .authorized(headers, unix_seconds()?)?
            .ok_or(AppError::Unauthorized)?;
        Ok(digest(b"reviewq-browser-csrf-v1", &token))
    }

    // The blocking pool serializes durable updates. Readers use the previous snapshot
    // while the new one is being written.
    pub(super) fn issue(&self, headers: &HeaderMap) -> Result<(String, HeaderValue), AppError> {
        let _issuance = self.issuance.lock().map_err(|_| AppError::Internal)?;
        let now = unix_seconds()?;
        let token = if let Some(existing) = self.authorized(headers, now)? {
            existing
        } else {
            let mut next = self.grants.read().map_err(|_| AppError::Internal)?.clone();
            next.expires.retain(|_, expiry| valid(*expiry, now));
            if next.expires.len() >= MAX_SESSIONS {
                return Err(AppError::Busy);
            }
            let token = random()?;
            let expiry = now.checked_add(SESSION_SECONDS).ok_or(AppError::Internal)?;
            if next.expires.insert(verifier(&token), expiry).is_some() {
                return Err(AppError::Internal);
            }
            (self.save)(&next)?;
            *self.grants.write().map_err(|_| AppError::Internal)? = next;
            token
        };
        let mut cookie = HeaderValue::from_str(&format!(
            "{}={}; Path=/api/v1; HttpOnly; SameSite=Strict",
            self.cookie,
            URL_SAFE_NO_PAD.encode(token)
        ))
        .map_err(|_| AppError::Internal)?;
        cookie.set_sensitive(true);
        Ok((
            URL_SAFE_NO_PAD.encode(digest(b"reviewq-browser-csrf-v1", &token)),
            cookie,
        ))
    }
}

fn valid(expiry: u64, now: u64) -> bool {
    expiry > now && expiry - now <= SESSION_SECONDS
}

fn verifier(token: &[u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(digest(b"reviewq-browser-grant-v1", token))
}

fn digest(domain: &'static [u8], value: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(domain)
        .chain_update([0])
        .chain_update(value)
        .finalize()
        .into()
}
