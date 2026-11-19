use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{HeaderMap, HeaderValue};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use reviewq_app::access::AppError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub(super) const MAX_CONTROL_AGE: u64 = 15;
const INSTANCE: &str = "x-reviewq-instance";
const NONCE: &str = "x-reviewq-nonce";
const ISSUED: &str = "x-reviewq-issued";
const PROOF: &str = "x-reviewq-control";
const BRIDGE: &str = "x-reviewq-native-bridge";

type HmacSha256 = Hmac<Sha256>;

/// Connection details stored in the private service descriptor.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceEndpoint {
    browser: SocketAddr,
    native: SocketAddr,
    instance: String,
    identity: ServiceIdentity,
    control_key: [u8; 32],
    bridge_key: [u8; 32],
    cursor_key: [u8; 32],
}

/// Owner-private locator for installed native callbacks.
#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeDescriptor {
    pub installation_id: String,
    native_address: SocketAddr,
    browser_url: String,
    instance: String,
    secret: String,
}

/// Identity established from the running executable, canonical profile and opened store.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceIdentity {
    pub build_digest: String,
    pub profile_id: String,
    pub store_id: String,
    pub store_application_id: i64,
    pub store_schema: i64,
}

pub struct ControlRequest<'a> {
    pub headers: HeaderMap,
    method: &'a str,
    target: &'a str,
    nonce: [u8; 32],
    issued: u64,
}

impl ServiceEndpoint {
    pub fn new(
        browser: SocketAddr,
        native: SocketAddr,
        identity: ServiceIdentity,
    ) -> Result<Self, AppError> {
        if !browser.ip().is_loopback() || !native.ip().is_loopback() {
            return Err(AppError::InvalidRequest);
        }
        Ok(Self {
            browser,
            native,
            instance: URL_SAFE_NO_PAD.encode(random()?),
            identity,
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
    pub fn identity(&self) -> &ServiceIdentity {
        &self.identity
    }

    pub fn bridge_descriptor(&self) -> BridgeDescriptor {
        BridgeDescriptor {
            installation_id: self.identity.profile_id.clone(),
            native_address: self.native,
            browser_url: format!("http://{}/", self.browser),
            instance: self.instance.clone(),
            secret: URL_SAFE_NO_PAD.encode(self.bridge_key),
        }
    }

    pub fn bridge_headers(&self) -> Result<HeaderMap, AppError> {
        let mut headers = HeaderMap::new();
        put(&mut headers, INSTANCE, &self.instance)?;
        put(
            &mut headers,
            BRIDGE,
            &URL_SAFE_NO_PAD.encode(self.bridge_key),
        )?;
        headers
            .get_mut(BRIDGE)
            .ok_or(AppError::Internal)?
            .set_sensitive(true);
        Ok(headers)
    }

    pub(super) fn verify_bridge(&self, headers: &HeaderMap) -> Result<(), AppError> {
        if headers.get_all(BRIDGE).iter().count() != 1
            || value(headers, INSTANCE) != Some(self.instance())
            || value(headers, "host") != Some(self.native.to_string().as_str())
            || headers.contains_key("origin")
        {
            return Err(AppError::Unauthorized);
        }
        let key = value(headers, BRIDGE)
            .and_then(decode)
            .ok_or(AppError::Unauthorized)?;
        if !bool::from(self.bridge_key.ct_eq(&key)) {
            return Err(AppError::Unauthorized);
        }
        Ok(())
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

    pub(super) fn encode_cursor<T: Serialize>(
        &self,
        scope: &[u8],
        position: &T,
    ) -> Result<String, AppError> {
        let mut bytes = serde_json::to_vec(position).map_err(|_| AppError::Internal)?;
        let mut mac = mac(&self.cursor_key);
        mac.update(scope);
        mac.update(&bytes);
        bytes.extend_from_slice(&mac.finalize().into_bytes());
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }

    pub(super) fn decode_cursor<T: DeserializeOwned>(
        &self,
        scope: &[u8],
        input: &str,
    ) -> Result<T, AppError> {
        if input.len() > 8192 {
            return Err(AppError::CursorExpired);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(input)
            .map_err(|_| AppError::CursorExpired)?;
        let (data, proof) = bytes
            .split_at_checked(bytes.len().checked_sub(32).ok_or(AppError::CursorExpired)?)
            .ok_or(AppError::CursorExpired)?;
        let mut mac = mac(&self.cursor_key);
        mac.update(scope);
        mac.update(data);
        mac.verify_slice(proof)
            .map_err(|_| AppError::CursorExpired)?;
        serde_json::from_slice(data).map_err(|_| AppError::CursorExpired)
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
