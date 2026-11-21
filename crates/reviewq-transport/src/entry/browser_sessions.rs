//! Stateless browser grants bound to one profile, store and origin.

use axum::http::{HeaderMap, HeaderValue, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use reviewq_app::access::AppError;
use sha2::{Digest, Sha256};

use super::control::{ServiceEndpoint, mac, random, unix_seconds};

const SESSION_SECONDS: u64 = 12 * 60 * 60;
const EXPIRY_BYTES: usize = 8;
const PAYLOAD_BYTES: usize = EXPIRY_BYTES + 32;
const SIGNED_BYTES: usize = PAYLOAD_BYTES + 32;
const ENCODED_BYTES: usize = (8 * SIGNED_BYTES).div_ceil(6);
const SESSION_DOMAIN: &[u8] = b"reviewq-browser-session-v1";
const CSRF_DOMAIN: &[u8] = b"reviewq-browser-csrf-v2";

/// Signs and verifies browser sessions for a single installation.
pub struct BrowserSessions {
    key: [u8; 32],
    scope: [u8; 32],
    cookie: String,
}

pub(super) struct BrowserSession([u8; PAYLOAD_BYTES]);

impl BrowserSessions {
    /// Bind the owner-private signing key to the opened profile, store and browser endpoint.
    pub fn new(
        endpoint: &ServiceEndpoint,
        directory_id: (u64, u64),
        database_id: (u64, u64),
        key: [u8; 32],
    ) -> Result<Self, AppError> {
        let identity = endpoint.identity();
        let binding = serde_json::to_vec(&(
            &identity.profile_id,
            &identity.store_id,
            endpoint.browser(),
            directory_id,
            database_id,
        ))
        .map_err(|_| AppError::Internal)?;
        let scope = Sha256::new()
            .chain_update(b"reviewq-browser-scope-v1")
            .chain_update([0])
            .chain_update(binding)
            .finalize()
            .into();
        let cookie = format!("reviewq_{}", URL_SAFE_NO_PAD.encode(scope));
        Ok(Self { key, scope, cookie })
    }

    pub(super) fn verify(&self, headers: &HeaderMap) -> Result<BrowserSession, AppError> {
        self.authorized(headers, unix_seconds()?)
            .ok_or(AppError::Unauthorized)
    }

    pub(super) fn csrf(&self, session: &BrowserSession) -> [u8; 32] {
        self.proof(CSRF_DOMAIN, &session.0)
            .finalize()
            .into_bytes()
            .into()
    }

    pub(super) fn issue(
        &self,
        headers: &HeaderMap,
    ) -> Result<([u8; 32], Option<HeaderValue>), AppError> {
        let now = unix_seconds()?;
        if let Some(session) = self.authorized(headers, now) {
            return Ok((self.csrf(&session), None));
        }

        let expiry = now.checked_add(SESSION_SECONDS).ok_or(AppError::Internal)?;
        let mut payload = [0; PAYLOAD_BYTES];
        payload[..EXPIRY_BYTES].copy_from_slice(&expiry.to_be_bytes());
        payload[EXPIRY_BYTES..].copy_from_slice(&random()?);

        let signature = self.proof(SESSION_DOMAIN, &payload).finalize().into_bytes();
        let mut signed = [0; SIGNED_BYTES];
        signed[..PAYLOAD_BYTES].copy_from_slice(&payload);
        signed[PAYLOAD_BYTES..].copy_from_slice(&signature);
        let mut cookie = HeaderValue::from_str(&format!(
            "{}={}; Path=/api/v1; HttpOnly; SameSite=Strict",
            self.cookie,
            URL_SAFE_NO_PAD.encode(signed)
        ))
        .map_err(|_| AppError::Internal)?;
        cookie.set_sensitive(true);
        Ok((self.csrf(&BrowserSession(payload)), Some(cookie)))
    }

    fn authorized(&self, headers: &HeaderMap, now: u64) -> Option<BrowserSession> {
        let mut cookies = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|cookie| cookie.split(';'))
            .filter_map(|item| item.trim().split_once('='))
            .filter_map(|(name, value)| (name == self.cookie).then_some(value));
        let encoded = cookies.next()?;
        if encoded.len() != ENCODED_BYTES || cookies.next().is_some() {
            return None;
        }
        let mut signed = [0; SIGNED_BYTES];
        if URL_SAFE_NO_PAD.decode_slice(encoded, &mut signed).ok()? != SIGNED_BYTES {
            return None;
        }
        let (payload, signature) = signed.split_at(PAYLOAD_BYTES);
        let payload: [u8; PAYLOAD_BYTES] = payload.try_into().ok()?;
        let expiry = u64::from_be_bytes(*payload.first_chunk()?);
        if expiry <= now || expiry - now > SESSION_SECONDS {
            return None;
        }
        self.proof(SESSION_DOMAIN, &payload)
            .verify_slice(signature)
            .ok()?;
        Some(BrowserSession(payload))
    }

    fn proof(&self, domain: &[u8], payload: &[u8; PAYLOAD_BYTES]) -> Hmac<Sha256> {
        let mut proof = mac(&self.key);
        proof.update(domain);
        proof.update(&[0]);
        proof.update(&self.scope);
        proof.update(payload);
        proof
    }
}
