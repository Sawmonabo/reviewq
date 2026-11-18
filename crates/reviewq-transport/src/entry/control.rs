use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{HeaderMap, HeaderValue};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use reviewq_app::access::AppError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) const MAX_CONTROL_AGE: u64 = 15;
const INSTANCE: &str = "x-reviewq-instance";
const NONCE: &str = "x-reviewq-nonce";
const ISSUED: &str = "x-reviewq-issued";
const PROOF: &str = "x-reviewq-control";

type HmacSha256 = Hmac<Sha256>;

/// Connection details stored in the private service descriptor.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceEndpoint {
    browser: SocketAddr,
    native: SocketAddr,
    instance: String,
    control_key: [u8; 32],
    bridge_key: [u8; 32],
    cursor_key: [u8; 32],
}

pub struct ControlRequest<'a> {
    pub headers: HeaderMap,
    method: &'a str,
    target: &'a str,
    nonce: [u8; 32],
    issued: u64,
}

impl ServiceEndpoint {
    pub fn new(browser: SocketAddr, native: SocketAddr) -> Result<Self, AppError> {
        if !browser.ip().is_loopback() || !native.ip().is_loopback() {
            return Err(AppError::InvalidRequest);
        }
        Ok(Self {
            browser,
            native,
            instance: URL_SAFE_NO_PAD.encode(random()?),
            control_key: random()?,
            bridge_key: random()?,
            cursor_key: random()?,
        })
    }

    pub fn browser(&self) -> SocketAddr {
        self.browser
    }
    pub fn native(&self) -> SocketAddr {
        self.native
    }
    pub fn instance(&self) -> &str {
        &self.instance
    }

    pub fn is_valid(&self) -> bool {
        self.browser.ip().is_loopback()
            && self.native.ip().is_loopback()
            && self.browser.port() != 0
            && self.native.port() != 0
            && decode(&self.instance).is_some()
    }

    pub fn sign<'a>(
        &self,
        method: &'a str,
        target: &'a str,
        body: &[u8],
    ) -> Result<ControlRequest<'a>, AppError> {
        let nonce = random()?;
        let issued = unix_seconds()?;
        let mac = self.message_mac(b"request", method, target, &nonce, issued, 0, body);
        let mut headers = HeaderMap::new();
        put(&mut headers, INSTANCE, &self.instance)?;
        put(&mut headers, NONCE, &URL_SAFE_NO_PAD.encode(nonce))?;
        put(&mut headers, ISSUED, &issued.to_string())?;
        put(
            &mut headers,
            PROOF,
            &URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()),
        )?;
        Ok(ControlRequest {
            headers,
            method,
            target,
            nonce,
            issued,
        })
    }

    pub fn verify_response(
        &self,
        request: &ControlRequest<'_>,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<(), AppError> {
        if value(headers, INSTANCE) != Some(self.instance()) {
            return Err(AppError::Unauthorized);
        }
        let proof = value(headers, PROOF)
            .and_then(decode)
            .ok_or(AppError::Unauthorized)?;
        self.message_mac(
            b"response",
            request.method,
            request.target,
            &request.nonce,
            request.issued,
            status,
            body,
        )
        .verify_slice(&proof)
        .map_err(|_| AppError::Unauthorized)
    }

    pub(super) fn verify_request<'a>(
        &self,
        method: &'a str,
        target: &'a str,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<ControlRequest<'a>, AppError> {
        if value(headers, INSTANCE) != Some(&self.instance) {
            return Err(AppError::Unauthorized);
        }
        let nonce = value(headers, NONCE)
            .and_then(decode)
            .ok_or(AppError::Unauthorized)?;
        let issued: u64 = value(headers, ISSUED)
            .and_then(|raw| raw.parse().ok())
            .ok_or(AppError::Unauthorized)?;
        let proof = value(headers, PROOF)
            .and_then(decode)
            .ok_or(AppError::Unauthorized)?;
        if unix_seconds()?.abs_diff(issued) > MAX_CONTROL_AGE {
            return Err(AppError::Unauthorized);
        }
        self.message_mac(b"request", method, target, &nonce, issued, 0, body)
            .verify_slice(&proof)
            .map_err(|_| AppError::Unauthorized)?;
        Ok(ControlRequest {
            headers: HeaderMap::new(),
            method,
            target,
            nonce,
            issued,
        })
    }

    pub(super) fn response_proof(
        &self,
        request: &ControlRequest<'_>,
        status: u16,
        headers: &mut HeaderMap,
        body: &[u8],
    ) -> Result<(), AppError> {
        put(headers, INSTANCE, &self.instance)?;
        let mac = self.message_mac(
            b"response",
            request.method,
            request.target,
            &request.nonce,
            request.issued,
            status,
            body,
        );
        put(
            headers,
            PROOF,
            &URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()),
        )
    }

    pub(super) fn cursor_mac(&self) -> HmacSha256 {
        mac(&self.cursor_key)
    }

    #[allow(clippy::too_many_arguments)]
    fn message_mac(
        &self,
        direction: &[u8],
        method: &str,
        target: &str,
        nonce: &[u8],
        issued: u64,
        status: u16,
        body: &[u8],
    ) -> HmacSha256 {
        let mut mac = mac(&self.control_key);
        let digest = Sha256::digest(body);
        for field in [
            b"reviewq-control-v1",
            direction,
            self.instance.as_bytes(),
            method.as_bytes(),
            target.as_bytes(),
            nonce,
            &issued.to_be_bytes(),
            &status.to_be_bytes(),
            digest.as_slice(),
        ] {
            mac.update(&(field.len() as u64).to_be_bytes());
            mac.update(field);
        }
        mac
    }
}

impl ControlRequest<'_> {
    pub(super) fn nonce(&self) -> [u8; 32] {
        self.nonce
    }
}

pub(super) fn mac(key: &[u8; 32]) -> HmacSha256 {
    HmacSha256::new_from_slice(key).expect("HMAC accepts every key length")
}

pub(super) fn random() -> Result<[u8; 32], AppError> {
    let mut token = [0_u8; 32];
    getrandom::fill(&mut token).map_err(|_| AppError::Internal)?;
    Ok(token)
}

pub(super) fn decode(raw: &str) -> Option<[u8; 32]> {
    if raw.len() != 43 {
        return None;
    }
    let mut result = [0_u8; 32];
    (URL_SAFE_NO_PAD.decode_slice(raw, &mut result).ok()? == 32).then_some(result)
}

fn put(headers: &mut HeaderMap, name: &'static str, raw: &str) -> Result<(), AppError> {
    let mut header = HeaderValue::from_str(raw).map_err(|_| AppError::Internal)?;
    if name == PROOF {
        header.set_sensitive(true);
    }
    headers.insert(name, header);
    Ok(())
}

pub(super) fn value<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

pub(super) fn unix_seconds() -> Result<u64, AppError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::Internal)?
        .as_secs())
}
