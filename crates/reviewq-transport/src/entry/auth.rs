use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, Method, Uri, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{Actor, AppError};
use serde::de::DeserializeOwned;
use subtle::ConstantTimeEq;

use super::control::{ControlRequest, MAX_CONTROL_AGE, ServiceEndpoint, decode, random, value};
use super::error::ApiError;

const MAX_NONCES: usize = 1024;
const FETCH_SITE: &str = "sec-fetch-site";
const CSRF: &str = "x-reviewq-csrf";

pub(super) struct Auth {
    pub(super) endpoint: ServiceEndpoint,
    pub(super) origin: String,
    browser_host: String,
    hosts: Vec<String>,
    csrf: [u8; 32],
    nonces: Mutex<HashMap<[u8; 32], Instant>>,
}

impl Auth {
    pub(super) fn new(endpoint: ServiceEndpoint) -> Result<Self, AppError> {
        let browser_host = endpoint.browser().to_string();
        Ok(Self {
            origin: format!("http://{browser_host}"),
            browser_host,
            hosts: endpoint.hosts(),
            endpoint,
            csrf: random()?,
            nonces: Mutex::default(),
        })
    }

    pub(super) fn allowed_host(&self, headers: &HeaderMap) -> bool {
        let host = value(headers, "host");
        host.is_some_and(|host| self.hosts.iter().any(|allowed| allowed == host))
            && headers.get_all(header::HOST).iter().count() == 1
            && headers.get_all(header::ORIGIN).iter().count() <= 1
            && (!headers.contains_key(header::ORIGIN)
                || value(headers, "origin") == Some(&self.origin))
    }

    /// Where a page read that reached another of ReviewQ's accepted addresses belongs: the same
    /// path and query on the browser's own origin, built from the endpoint, never from the
    /// request. The browser's API answers only there, so a page opened at `localhost` or
    /// `[::1]` moves instead of loading and failing. `None` for the browser's own address,
    /// the API, `/mcp` and anything but `GET` or `HEAD`.
    pub(super) fn page_elsewhere(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
    ) -> Option<String> {
        let path = uri.path();
        if !matches!(*method, Method::GET | Method::HEAD)
            || path == "/mcp"
            || path.starts_with("/api/")
            || value(headers, "host") == Some(&self.browser_host)
        {
            return None;
        }
        let target = uri.path_and_query().map_or(path, |target| target.as_str());
        Some(format!("{}{target}", self.origin))
    }

    pub(super) fn control<'a>(
        &self,
        method: &'a str,
        url: &'a str,
        headers: &HeaderMap,
        bytes: &[u8],
    ) -> Result<ControlRequest<'a>, AppError> {
        let request = self.endpoint.verify_request(method, url, headers, bytes)?;
        let mut nonces = self.nonces.lock().map_err(|_| AppError::Internal)?;
        let now = Instant::now();
        nonces.retain(|_, deadline| *deadline > now);
        if nonces.contains_key(&request.nonce()) {
            return Err(AppError::Unauthorized);
        }
        if nonces.len() >= MAX_NONCES {
            return Err(AppError::Busy);
        }
        nonces.insert(
            request.nonce(),
            now + Duration::from_secs(MAX_CONTROL_AGE * 2 + 1),
        );
        Ok(request)
    }

    /// Reads a native bridge request: the bridge credential, a JSON content type and a body of
    /// at most `limit` bytes, decoded as `T`. Every native bridge route calls it first.
    ///
    /// Fails with the credential's error, or with `InvalidRequest` for another content type,
    /// a longer body or a body that does not decode.
    pub(super) fn bridge_command<T: DeserializeOwned>(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        limit: usize,
    ) -> Result<T, AppError> {
        self.endpoint.verify_bridge(headers)?;
        if body.len() > limit
            || !headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.split(';').next() == Some("application/json"))
        {
            return Err(AppError::InvalidRequest);
        }
        serde_json::from_slice(body).map_err(|_| AppError::InvalidRequest)
    }

    pub(super) fn browser_actor(
        &self,
        headers: &HeaderMap,
        mutation: bool,
    ) -> Result<Actor, ApiError> {
        if mutation {
            self.require_browser_origin(headers)?;
            let supplied = value(headers, CSRF)
                .filter(|_| headers.get_all(CSRF).iter().count() == 1)
                .ok_or(ApiError::BrowserProofRejected)?;
            if !self.valid_browser_proof(supplied) {
                return Err(ApiError::BrowserProofRejected);
            }
        } else {
            self.check_browser(headers)?;
        }
        Ok(Actor::local_operator())
    }

    pub(super) fn require_browser_origin(&self, headers: &HeaderMap) -> Result<(), AppError> {
        self.check_browser(headers)?;
        if value(headers, "origin") != Some(&self.origin)
            || headers.get_all(header::ORIGIN).iter().count() != 1
        {
            return Err(AppError::Forbidden);
        }
        Ok(())
    }

    pub(super) fn valid_browser_proof(&self, supplied: &str) -> bool {
        decode(supplied).is_some_and(|bytes| bool::from(self.csrf.ct_eq(&bytes)))
    }

    pub(super) fn csrf(&self, headers: &HeaderMap) -> Result<String, AppError> {
        self.check_browser(headers)?;
        let same_origin = if headers.contains_key(FETCH_SITE) {
            value(headers, FETCH_SITE) == Some("same-origin")
        } else {
            // A GET may omit Origin. Older browsers still send a same-origin Referer.
            value(headers, "origin") == Some(&self.origin)
                || value(headers, "referer")
                    .and_then(|referer| referer.strip_prefix(&self.origin))
                    .is_some_and(|path| path.starts_with('/'))
        };
        if !same_origin {
            return Err(AppError::Forbidden);
        }
        Ok(URL_SAFE_NO_PAD.encode(self.csrf))
    }

    fn check_browser(&self, headers: &HeaderMap) -> Result<(), AppError> {
        let mut sites = headers.get_all(FETCH_SITE).iter();
        let origin_allowed = sites
            .next()
            .is_none_or(|site| site == "same-origin" || site == "none");
        if value(headers, "host") != Some(&self.browser_host)
            || !origin_allowed
            || sites.next().is_some()
        {
            return Err(AppError::Forbidden);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderValue, StatusCode};
    use axum::response::IntoResponse;

    use super::*;
    use crate::entry::ServiceIdentity;

    #[test]
    fn old_browser_proof_is_rejected_at_the_gate_after_restart() {
        let endpoint = ServiceEndpoint::new(
            "127.0.0.1:8766".parse().unwrap(),
            "[::1]:8766".parse().unwrap(),
            ServiceIdentity {
                build_digest: String::new(),
                executable_file: String::new(),
                profile_id: String::new(),
                store_id: String::new(),
                store_application_id: 0,
                store_schema: 0,
            },
        )
        .unwrap();
        let before = Auth::new(endpoint.clone()).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8766"));
        assert!(before.browser_actor(&headers, false).is_ok());
        assert!(before.csrf(&headers).is_err());

        headers.insert(FETCH_SITE, HeaderValue::from_static("same-origin"));
        let old_proof = before.csrf(&headers).unwrap();
        headers.insert(CSRF, old_proof.parse().unwrap());
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:8766"),
        );
        assert!(before.browser_actor(&headers, true).is_ok());

        let after = Auth::new(endpoint).unwrap();
        let response = after
            .browser_actor(&headers, true)
            .unwrap_err()
            .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(response.headers()["x-reviewq-csrf-rejected"], "1");

        let new_proof = after.csrf(&headers).unwrap();
        headers.insert(CSRF, new_proof.parse().unwrap());
        assert!(after.browser_actor(&headers, true).is_ok());

        headers.insert(FETCH_SITE, HeaderValue::from_static("same-site"));
        assert!(after.csrf(&headers).is_err());
        let response = after
            .browser_actor(&headers, true)
            .unwrap_err()
            .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response.headers().contains_key("x-reviewq-csrf-rejected"));
    }
}
