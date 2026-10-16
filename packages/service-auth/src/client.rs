//! Authenticated internal HTTP client (feature `client`).
//!
//! Generic transport for synchronous service-to-service calls. It contains no domain
//! operations. For every call it:
//!
//! * selects the target from the token audience — the base URL comes from this service's
//!   configuration for that service identity, never from business code;
//! * signs a fresh short-lived token bound to the hop's request id, the operation's
//!   correlation id and the optional delegated context;
//! * restricts paths to the `/internal/v1/` namespace;
//! * requires HTTPS outside `local` / `test` runtime environments;
//! * never follows redirects (a token is never forwarded to another host), bypasses
//!   ambient HTTP proxies, applies timeouts and bounds the response size;
//! * maps failures to safe errors that never contain tokens, URLs or bodies.

use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use http::{HeaderMap, Method};
use serde::{de::DeserializeOwned, Serialize};
use url::Url;

use crate::{
    headers,
    identity::{RuntimeEnvironment, ServiceId},
    ids::RequestId,
    signer::{OutboundServiceRequest, ServiceTokenSigner, SignError},
};

/// Internal API namespace every internal route lives under.
pub const INTERNAL_PATH_PREFIX: &str = "/internal/v1/";
/// Maximum accepted response body.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientConfigError {
    #[error("base URL for {service} is invalid")]
    InvalidBaseUrl { service: ServiceId },
    #[error("base URL for {service} must use https in the {environment} runtime environment")]
    InsecureBaseUrl {
        service: ServiceId,
        environment: RuntimeEnvironment,
    },
    #[error("a service cannot target itself ({0})")]
    SelfTarget(ServiceId),
    #[error("target {0} is configured twice")]
    DuplicateTarget(ServiceId),
    #[error("targets are configured for {targets} but the signer runs in {signer}")]
    EnvironmentMismatch {
        targets: RuntimeEnvironment,
        signer: RuntimeEnvironment,
    },
    #[error("the HTTP client cannot be constructed")]
    Http,
}

/// Configured base URLs of the services this service may call.
#[derive(Debug, Clone)]
pub struct InternalTargets {
    environment: RuntimeEnvironment,
    targets: BTreeMap<ServiceId, Url>,
}

impl InternalTargets {
    pub fn new(environment: RuntimeEnvironment) -> Self {
        Self {
            environment,
            targets: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, service: ServiceId, base_url: &str) -> Result<(), ClientConfigError> {
        let url = Url::parse(base_url).map_err(|_| ClientConfigError::InvalidBaseUrl {
            service: service.clone(),
        })?;
        let shape_ok = url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none();
        if !shape_ok {
            return Err(ClientConfigError::InvalidBaseUrl { service });
        }
        match url.scheme() {
            "https" => {}
            "http" if self.environment.allows_insecure_transport() => {}
            "http" => {
                return Err(ClientConfigError::InsecureBaseUrl {
                    service,
                    environment: self.environment,
                })
            }
            _ => return Err(ClientConfigError::InvalidBaseUrl { service }),
        }
        if self.targets.contains_key(&service) {
            return Err(ClientConfigError::DuplicateTarget(service));
        }
        self.targets.insert(service, url);
        Ok(())
    }

    pub fn contains(&self, service: &ServiceId) -> bool {
        self.targets.contains_key(service)
    }

    pub fn services(&self) -> impl Iterator<Item = &ServiceId> {
        self.targets.keys()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InternalCallError {
    #[error("no base URL is configured for {0}")]
    UnknownTarget(ServiceId),
    #[error("internal paths must be normalized paths under {INTERNAL_PATH_PREFIX}")]
    InvalidPath,
    #[error("{0}")]
    Signing(SignError),
    #[error("request body could not be encoded")]
    Encoding,
    #[error("internal call to {target} timed out")]
    Timeout { target: ServiceId },
    #[error("internal call to {target} could not connect")]
    Connect { target: ServiceId },
    #[error("internal call to {target} failed in transport")]
    Transport { target: ServiceId },
    #[error("internal call to {target} returned HTTP {status}{}", code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Status {
        target: ServiceId,
        status: u16,
        /// Stable error code from the response envelope, when present and well-formed.
        code: Option<String>,
    },
    #[error(
        "internal call to {target} returned a response larger than {MAX_RESPONSE_BYTES} bytes"
    )]
    ResponseTooLarge { target: ServiceId },
    #[error("internal call to {target} returned an undecodable response")]
    InvalidResponse { target: ServiceId },
}

/// Response of an internal call (2xx unless obtained through `exchange_json`).
pub struct InternalResponse {
    target: ServiceId,
    status: u16,
    request_id: RequestId,
    /// Response headers relayed to callers (bounded allow-list; never auth material).
    relayed: BTreeMap<&'static str, String>,
    body: Vec<u8>,
}

/// Response headers an adapter may relay (never authentication material).
pub const RELAYED_RESPONSE_HEADERS: [&str; 1] = ["idempotent-replayed"];

impl fmt::Debug for InternalResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InternalResponse")
            .field("target", &self.target)
            .field("status", &self.status)
            .field("request_id", &self.request_id)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl InternalResponse {
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Request id of the hop this response answers.
    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    /// Stable error code of an error envelope body (`{"error":{"code":…}}`), when present
    /// and well-formed (upper-case letters and `_`, at most 64 characters).
    pub fn error_code(&self) -> Option<String> {
        serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| {
                v.pointer("/error/code")
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            })
            .filter(|c| c.len() <= 64 && c.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
    }

    /// A relayed response header (see [`RELAYED_RESPONSE_HEADERS`]).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.relayed.get(name).map(String::as_str)
    }

    /// The bounded response body.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, InternalCallError> {
        serde_json::from_slice(&self.body).map_err(|_| InternalCallError::InvalidResponse {
            target: self.target.clone(),
        })
    }
}

pub struct InternalServiceClient {
    http: reqwest::Client,
    signer: Arc<ServiceTokenSigner>,
    targets: InternalTargets,
}

impl fmt::Debug for InternalServiceClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InternalServiceClient")
            .field("issuer", self.signer.issuer())
            .field("targets", &self.targets.targets.keys().collect::<Vec<_>>())
            .finish()
    }
}

fn valid_internal_path(path: &str) -> bool {
    path.starts_with(INTERNAL_PATH_PREFIX)
        && path.len() > INTERNAL_PATH_PREFIX.len()
        && path.len() <= 512
        && !path.contains("//")
        && !path.split('/').any(|s| s == "." || s == "..")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~'))
}

impl InternalServiceClient {
    pub fn new(
        signer: Arc<ServiceTokenSigner>,
        targets: InternalTargets,
        timeout: Duration,
    ) -> Result<Self, ClientConfigError> {
        if targets.environment != signer.runtime_environment() {
            return Err(ClientConfigError::EnvironmentMismatch {
                targets: targets.environment,
                signer: signer.runtime_environment(),
            });
        }
        if targets.contains(signer.issuer()) {
            return Err(ClientConfigError::SelfTarget(signer.issuer().clone()));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .build()
            .map_err(|_| ClientConfigError::Http)?;
        Ok(Self {
            http,
            signer,
            targets,
        })
    }

    pub async fn get(
        &self,
        request: &OutboundServiceRequest,
        path: &str,
    ) -> Result<InternalResponse, InternalCallError> {
        self.send(request, Method::GET, path, None).await
    }

    pub async fn post_json<B: Serialize>(
        &self,
        request: &OutboundServiceRequest,
        path: &str,
        body: &B,
    ) -> Result<InternalResponse, InternalCallError> {
        let bytes = serde_json::to_vec(body).map_err(|_| InternalCallError::Encoding)?;
        self.send(request, Method::POST, path, Some(bytes)).await
    }

    /// Like [`post_json`](Self::post_json) but returns every HTTP status as a response
    /// (bounded body, never followed redirects). For adapters that relay a downstream
    /// service's own error envelope (e.g. an authenticated platform ingress).
    pub async fn exchange_json<B: Serialize>(
        &self,
        request: &OutboundServiceRequest,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<InternalResponse, InternalCallError> {
        let bytes = match body {
            Some(body) => Some(serde_json::to_vec(body).map_err(|_| InternalCallError::Encoding)?),
            None => None,
        };
        self.send_raw(request, method, path, bytes).await
    }

    async fn send(
        &self,
        request: &OutboundServiceRequest,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<InternalResponse, InternalCallError> {
        let response = self.send_raw(request, method, path, body).await?;
        let status = response.status;
        if !(200..300).contains(&status) {
            let code = response.error_code();
            return Err(InternalCallError::Status {
                target: response.target,
                status,
                code,
            });
        }
        Ok(response)
    }

    async fn send_raw(
        &self,
        request: &OutboundServiceRequest,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<InternalResponse, InternalCallError> {
        let target = request.audience().clone();
        let base = self
            .targets
            .targets
            .get(&target)
            .ok_or_else(|| InternalCallError::UnknownTarget(target.clone()))?;
        if !valid_internal_path(path) {
            return Err(InternalCallError::InvalidPath);
        }
        let url = base
            .join(path)
            .map_err(|_| InternalCallError::InvalidPath)?;
        let signed = self
            .signer
            .sign(request)
            .map_err(InternalCallError::Signing)?;

        let mut header_map = HeaderMap::new();
        signed.apply_to(&mut header_map);
        let mut builder = self.http.request(method, url).headers(header_map);
        if let Some(body) = body {
            builder = builder
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        let mut response = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                InternalCallError::Timeout {
                    target: target.clone(),
                }
            } else if e.is_connect() {
                InternalCallError::Connect {
                    target: target.clone(),
                }
            } else {
                InternalCallError::Transport {
                    target: target.clone(),
                }
            }
        })?;
        let status = response.status().as_u16();
        let relayed = RELAYED_RESPONSE_HEADERS
            .iter()
            .filter_map(|name| {
                let value = response.headers().get(*name)?.to_str().ok()?;
                (value.len() <= 64).then(|| (*name, value.to_string()))
            })
            .collect();
        let mut buffer = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if buffer.len() + chunk.len() > MAX_RESPONSE_BYTES {
                        return Err(InternalCallError::ResponseTooLarge { target });
                    }
                    buffer.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) if e.is_timeout() => return Err(InternalCallError::Timeout { target }),
                Err(_) => return Err(InternalCallError::Transport { target }),
            }
        }
        Ok(InternalResponse {
            target,
            status,
            request_id: signed.request_id().clone(),
            relayed,
            body: buffer,
        })
    }
}

/// Header names an internal request carries (for transports that build requests
/// themselves).
pub const INTERNAL_HEADERS: [http::HeaderName; 3] = [
    headers::SERVICE_AUTHORIZATION,
    headers::REQUEST_ID,
    headers::CORRELATION_ID,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_paths_are_restricted() {
        assert!(valid_internal_path("/internal/v1/communications/authorize"));
        for bad in [
            "/internal/v1/",
            "/api/v1/x",
            "/internal/v2/x",
            "/internal/v1/../admin",
            "/internal/v1//x",
            "/internal/v1/x?y=1",
            "/internal/v1/x#f",
            "/internal/v1/x y",
            "https://evil/internal/v1/x",
        ] {
            assert!(!valid_internal_path(bad), "{bad}");
        }
    }

    #[test]
    fn target_urls_are_validated() {
        let mut staging = InternalTargets::new(RuntimeEnvironment::Staging);
        assert!(staging
            .insert(ServiceId::privacy_relay(), "https://privacy-relay.internal")
            .is_ok());
        assert!(matches!(
            staging.insert(ServiceId::telephony(), "http://telephony.internal"),
            Err(ClientConfigError::InsecureBaseUrl { .. })
        ));
        for bad in [
            "https://user:pass@x.internal",
            "https://x.internal/?q=1",
            "ftp://x.internal",
            "not a url",
        ] {
            assert!(
                staging.insert(ServiceId::telephony(), bad).is_err(),
                "{bad}"
            );
        }
        assert!(matches!(
            staging.insert(ServiceId::privacy_relay(), "https://again.internal"),
            Err(ClientConfigError::DuplicateTarget(_))
        ));
        let mut local = InternalTargets::new(RuntimeEnvironment::Local);
        assert!(local
            .insert(ServiceId::telephony(), "http://127.0.0.1:9000")
            .is_ok());
    }
}
