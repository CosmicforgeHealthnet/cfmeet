//! Notification Service user relay (Document 4 §3.1, U1–U9).
//!
//! The inbox, unread count, preferences and push-endpoint APIs of the unified Notification
//! Service are served to CF Meet users under `/api/v2/notifications`, like
//! `/api/v2/communications/*`: Meeting verifies the Account session
//! ([`crate::handlers::v2::notifications`]) and forwards the request to I2
//! `POST /internal/v1/notifications/dispatch` (`notifications-user.forward`, grantable to
//! `cfmeet.meeting-service` only) with the server-resolved account
//! (`cfmeet-user:<user id>`) — never an account the client names. The envelope carries the
//! method, the path relative to `/api/v2/notifications` (`""` for the list), the raw query
//! string, the `Idempotency-Key` value and the JSON body. The answer's status and body
//! (`schemaVersion: 1`, checked) are returned, with its `Retry-After` and `ETag`.
//!
//! The hop is signed with the Meeting signing identity for audience
//! `cfmeet.notifications` (fresh single-audience token, request-bound when emission is on).
//! The transport is the shared one's rules (no redirects, no ambient proxy, timeouts,
//! bounded answers) with one difference: it reads the `Retry-After` and `ETag` answer
//! headers the user API relays.
//!
//! Dark by default: without `CF_NOTIFICATIONS_BASE_URL` every route answers 503
//! `SERVICE_UNAVAILABLE` after authenticating the caller, and nothing is forwarded.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `CF_NOTIFICATIONS_BASE_URL` | Notification Service base URL (unset: relay off) |
//! | `CF_RUNTIME_ENVIRONMENT` | runtime environment of the signer (required with the URL) |
//! | `CF_SERVICE_AUTH_KEY_ID` | Meeting signing key id (required with the URL) |
//! | `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | Meeting signing key, PKCS#8 PEM (required with the URL) |

use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::http::{HeaderMap, Method};
use cf_service_auth::{
    client::InternalTargets, KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId,
    ServiceSigningKey, ServiceTokenSigner, SystemClock, TokenTtl,
};
use serde::Serialize;
use serde_json::Value;
use url::Url;

/// I2 `dispatchNotificationsUser`.
pub const DISPATCH_PATH: &str = "/internal/v1/notifications/dispatch";
/// The Notification Service's Svix-verified Resend callback (Document 4 §3.2).
pub const RESEND_EVENTS_PATH: &str = "/api/v1/notifications/provider-events/resend";
/// The provider headers a forwarded event keeps (the service verifies the signature itself).
pub const SVIX_HEADERS: [&str; 3] = ["svix-id", "svix-timestamp", "svix-signature"];
/// Largest relayed answer (a page of at most 100 inbox items).
const MAX_ANSWER_BYTES: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

/// The I2 relay envelope (`notifications/internal/v1.openapi.json`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dispatch {
    pub method: String,
    /// Relative to `/api/v2/notifications` (`""` for the list).
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// `cfmeet-user:<uuid>`, resolved by Meeting from the verified session.
    pub account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// The Notification Service's answer to one dispatch.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
    /// `Retry-After` in seconds (digits only).
    pub retry_after: Option<String>,
    /// `ETag` (visible ASCII, at most 128 bytes).
    pub etag: Option<String>,
}

/// Why a dispatch produced no answer (never contains tokens, URLs or bodies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayUnavailable;

pub struct NotificationsRelay {
    signer: Arc<ServiceTokenSigner>,
    url: Url,
    resend_events_url: Url,
    http: reqwest::Client,
}

impl std::fmt::Debug for NotificationsRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationsRelay").finish_non_exhaustive()
    }
}

impl NotificationsRelay {
    /// `Ok(None)`: not configured (the routes answer 503). `Err`: configured but invalid
    /// (startup fails closed).
    pub fn from_env() -> Result<Option<Arc<Self>>, String> {
        let Some(url) = std::env::var("CF_NOTIFICATIONS_BASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
        else {
            return Ok(None);
        };
        let required = |k: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| format!("{k} is required with CF_NOTIFICATIONS_BASE_URL"))
        };
        let runtime = RuntimeEnvironment::parse(&required("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT must be local, test, staging or production")?;
        let key = ServiceSigningKey::from_pkcs8_pem_file(
            KeyId::parse(&required("CF_SERVICE_AUTH_KEY_ID")?)
                .map_err(|_| "invalid CF_SERVICE_AUTH_KEY_ID")?,
            &PathBuf::from(required("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?),
        )
        .map_err(|_| "the Meeting signing key is unavailable")?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            runtime,
            key,
            TokenTtl::seconds(60).map_err(|_| "invalid token TTL")?,
            Arc::new(SystemClock),
        ));
        Self::new(signer, url.trim()).map(|r| Some(Arc::new(r)))
    }

    /// A relay signing with `signer` (the Meeting identity) to the Notification Service at
    /// `base_url` (HTTPS outside `local` / `test`).
    pub fn new(signer: Arc<ServiceTokenSigner>, base_url: &str) -> Result<Self, String> {
        if signer.issuer() != &ServiceId::meeting_service() {
            return Err("the relay signs as cfmeet.meeting-service".into());
        }
        // The shared base-URL rules (shape, HTTPS outside local / test).
        InternalTargets::new(signer.runtime_environment())
            .insert(ServiceId::notifications(), base_url)
            .map_err(|e| format!("CF_NOTIFICATIONS_BASE_URL: {e}"))?;
        let url = Url::parse(base_url)
            .and_then(|u| u.join(DISPATCH_PATH))
            .map_err(|_| "CF_NOTIFICATIONS_BASE_URL is invalid")?;
        let resend_events_url = Url::parse(base_url)
            .and_then(|u| u.join(RESEND_EVENTS_PATH))
            .map_err(|_| "CF_NOTIFICATIONS_BASE_URL is invalid")?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(TIMEOUT)
            .connect_timeout(TIMEOUT)
            .build()
            .map_err(|_| "the Notification Service HTTP client cannot be built")?;
        Ok(Self {
            signer,
            url,
            resend_events_url,
            http,
        })
    }

    /// Hands one Svix-signed Resend event, byte for byte with its three `svix-*` headers, to
    /// the Notification Service's provider callback (Document 4 §3.2). The provider account
    /// has one webhook, registered on the Meeting Service; events for messages the Meeting
    /// ledger did not send belong to the Notification Service, which verifies the same
    /// signature with the same secret. Returns the answer's status; transport failures are
    /// [`RelayUnavailable`] (the caller answers 503 so the provider retries).
    pub async fn forward_resend_event(
        &self,
        incoming: &HeaderMap,
        body: &[u8],
    ) -> Result<u16, RelayUnavailable> {
        let mut headers = HeaderMap::new();
        for name in SVIX_HEADERS {
            if let Some(value) = incoming.get(name) {
                headers.insert(name, value.clone());
            }
        }
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        let response = self
            .http
            .post(self.resend_events_url.clone())
            .headers(headers)
            .body(body.to_vec())
            .send()
            .await
            .map_err(|_| RelayUnavailable)?;
        Ok(response.status().as_u16())
    }

    /// One signed I2 call. Every HTTP status is an answer; transport failures, oversize
    /// answers and signing failures are [`RelayUnavailable`].
    pub async fn dispatch(&self, envelope: &Dispatch) -> Result<Answer, RelayUnavailable> {
        let signed = self
            .signer
            .sign_request(
                &OutboundServiceRequest::new_operation(ServiceId::notifications()),
                &Method::POST,
                self.url.path(),
            )
            .map_err(|_| RelayUnavailable)?;
        let mut headers = HeaderMap::new();
        signed.apply_to(&mut headers);
        let mut response = self
            .http
            .post(self.url.clone())
            .headers(headers)
            .json(envelope)
            .send()
            .await
            .map_err(|_| RelayUnavailable)?;
        let header = |name: &str, ok: &dyn Fn(&str) -> bool| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| ok(v))
                .map(str::to_owned)
        };
        let retry_after = header("retry-after", &|v| {
            !v.is_empty() && v.len() <= 10 && v.bytes().all(|b| b.is_ascii_digit())
        });
        let etag = header("etag", &|v| {
            !v.is_empty() && v.len() <= 128 && v.bytes().all(|b| (0x21..=0x7e).contains(&b))
        });
        let status = response.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| RelayUnavailable)? {
            if body.len() + chunk.len() > MAX_ANSWER_BYTES {
                return Err(RelayUnavailable);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Answer {
            status,
            body,
            retry_after,
            etag,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer(id: ServiceId, environment: RuntimeEnvironment) -> Arc<ServiceTokenSigner> {
        Arc::new(ServiceTokenSigner::new(
            id,
            environment,
            ServiceSigningKey::generate(KeyId::parse("meeting-service-test-1").unwrap()),
            TokenTtl::default(),
            Arc::new(SystemClock),
        ))
    }

    #[test]
    fn the_envelope_omits_what_the_request_did_not_carry() {
        let d = Dispatch {
            method: "GET".into(),
            path: String::new(),
            query: None,
            account: "cfmeet-user:00000000-0000-4000-8000-000000000001".into(),
            idempotency_key: None,
            body: None,
        };
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            serde_json::json!({ "method": "GET", "path": "",
                "account": "cfmeet-user:00000000-0000-4000-8000-000000000001" })
        );
        let d = Dispatch {
            method: "POST".into(),
            path: "/read".into(),
            query: Some("a=1".into()),
            idempotency_key: Some("key-12345".into()),
            body: Some(serde_json::json!({ "upTo": "x" })),
            ..d
        };
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            serde_json::json!({ "method": "POST", "path": "/read", "query": "a=1",
                "account": "cfmeet-user:00000000-0000-4000-8000-000000000001",
                "idempotencyKey": "key-12345", "body": { "upTo": "x" } })
        );
    }

    #[test]
    fn the_base_url_and_identity_are_checked() {
        let test = signer(ServiceId::meeting_service(), RuntimeEnvironment::Test);
        let relay = NotificationsRelay::new(test.clone(), "http://127.0.0.1:9").unwrap();
        assert_eq!(
            relay.url.as_str(),
            "http://127.0.0.1:9/internal/v1/notifications/dispatch"
        );
        assert_eq!(
            relay.resend_events_url.as_str(),
            "http://127.0.0.1:9/api/v1/notifications/provider-events/resend"
        );
        assert!(NotificationsRelay::new(test, "not a url").is_err());
        let production = signer(ServiceId::meeting_service(), RuntimeEnvironment::Production);
        assert!(NotificationsRelay::new(production.clone(), "http://notifications:8456").is_err());
        assert!(NotificationsRelay::new(production, "https://notifications:8456").is_ok());
        let other = signer(ServiceId::calling(), RuntimeEnvironment::Test);
        assert!(NotificationsRelay::new(other, "http://127.0.0.1:9").is_err());
    }
}
