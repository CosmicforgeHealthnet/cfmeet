//! ADR 0063 N-P1: the Notification Service's partner push provider vault, as the
//! Developer Console's upload path uses it (`notifications-push-provider.manage`).
//!
//! The Developer Console never keeps a push secret. On receipt the APNs `.p8` key or the
//! FCM service-account JSON is sealed ([`seal_secret`]) to the Notification Service's
//! public HPKE key with `packages/notification-client` `seal`, forwarded over signed
//! service auth, and dropped; only metadata and a SHA-256 fingerprint are stored here
//! (`devplat_push_providers`).
//!
//! Notification Service routes (build contract §6 N-P1), scope-locked to one
//! `(tenant, application, environment)`:
//!
//! * `PUT /internal/v1/push/providers/{tenantId}/{applicationId}/{environmentId}/{provider}`
//!   `{sealed, metadata, fingerprint}` → `{provider, status, fingerprint, updatedAt}`;
//! * `DELETE …/{provider}`; `POST …/{provider}/verify` → `{status, lastError?}`.
//!
//! ## The `sealed` value
//!
//! A sealed part holds at most 512 base64url characters (`SealedPlaintext.token`), which
//! an FCM service-account JSON exceeds. So the secret is base64url-encoded (no padding),
//! split into `n` near-equal chunks of at most 512 characters, and each chunk is sealed
//! as the `token` of its own part. `sealed` is the compact JSON string
//! `{"v":1,"parts":[{"kid","enc","ct"},…]}`; part `i` (0-based) is sealed with the AAD
//! `(producer, eventId, eventType)` = (`cfmeet.developer-platform`,
//! `<tenantId>/<applicationId>/<environmentId>/<provider>`,
//! `push-provider.secret.<i>-of-<n>`). The Notification Service recomputes the AAD from
//! the route, so a sealed value replayed to another scope or provider does not open, and
//! parts cannot be reordered or dropped. Concatenating the opened tokens and decoding
//! them yields the secret bytes (the `.p8` PEM text or the JSON text).

use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine as _};
use cf_notification_client::{
    seal::{seal, SealError},
    SealedPlaintext, SealingKey,
};
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, OutboundServiceRequest, ServiceId,
};
use serde_json::{json, Value};
use uuid::Uuid;

/// The AAD producer of every sealed push secret.
pub const SEAL_PRODUCER: &str = "cfmeet.developer-platform";
/// Characters of base64url secret per sealed part (the `token` maximum).
pub const MAX_PART_CHARS: usize = 512;
/// Largest secret accepted (bytes).
pub const MAX_SECRET_BYTES: usize = 16 * 1024;

/// The scope a push credential is locked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

impl PushScope {
    fn path(&self, provider: &str) -> String {
        format!(
            "/internal/v1/push/providers/{}/{}/{}/{provider}",
            self.tenant_id, self.application_id, self.environment_id
        )
    }

    /// The AAD event id of a sealed secret for `provider`.
    pub fn seal_event_id(&self, provider: &str) -> String {
        format!(
            "{}/{}/{}/{provider}",
            self.tenant_id, self.application_id, self.environment_id
        )
    }
}

/// The AAD event type of part `index` of `count`.
pub fn seal_event_type(index: usize, count: usize) -> String {
    format!("push-provider.secret.{index}-of-{count}")
}

/// Seals `secret` for `(scope, provider)`: the `sealed` string of the N-P1 PUT body.
pub fn seal_secret(
    key: &SealingKey,
    scope: &PushScope,
    provider: &str,
    secret: &[u8],
) -> Result<String, SealError> {
    if secret.is_empty() || secret.len() > MAX_SECRET_BYTES {
        return Err(SealError::InvalidPlaintext);
    }
    let encoded = B64.encode(secret);
    let count = encoded.len().div_ceil(MAX_PART_CHARS);
    let size = encoded.len().div_ceil(count);
    let event_id = scope.seal_event_id(provider);
    let mut parts = Vec::with_capacity(count);
    for (index, chunk) in encoded.as_bytes().chunks(size).enumerate() {
        // base64url is ASCII: every chunk is valid UTF-8.
        let chunk = std::str::from_utf8(chunk).map_err(|_| SealError::InvalidPlaintext)?;
        let plaintext = SealedPlaintext::default().with_token(chunk);
        let sealed = seal(
            key,
            SEAL_PRODUCER,
            &event_id,
            &seal_event_type(index, count),
            &plaintext,
        )?;
        parts.push(sealed);
    }
    if parts.len() != count {
        return Err(SealError::InvalidPlaintext);
    }
    Ok(json!({ "v": 1, "parts": parts }).to_string())
}

/// The outcome of a vault operation.
#[derive(Debug, Clone, PartialEq)]
pub enum PushReply {
    /// 2xx (`null` for `204`).
    Ok(Value),
    /// The Notification Service refused (stable code; never a reason to retry).
    Refused { status: u16, code: String },
}

#[derive(Debug, thiserror::Error)]
#[error("the notification service is unavailable: {0}")]
pub struct PushUnavailable(pub String);

#[async_trait::async_trait]
pub trait PushProviderPort: Send + Sync {
    /// Stores (replaces) the sealed credential of `provider` (`APNS`, `APNS_VOIP`, `FCM`).
    async fn put(
        &self,
        scope: &PushScope,
        provider: &str,
        body: &Value,
        correlation: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable>;

    async fn delete(
        &self,
        scope: &PushScope,
        provider: &str,
        correlation: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable>;

    async fn verify(
        &self,
        scope: &PushScope,
        provider: &str,
        correlation: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable>;
}

/// The vault and the key secrets are sealed to: both are needed to accept an upload.
#[derive(Clone)]
pub struct PushProviders {
    pub port: Arc<dyn PushProviderPort>,
    pub sealing_key: SealingKey,
}

/// The Notification Service over authenticated internal HTTP.
pub struct HttpPushProviders {
    client: Arc<InternalServiceClient>,
}

impl HttpPushProviders {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }

    async fn call(
        &self,
        method: axum::http::Method,
        path: String,
        body: Option<&Value>,
        correlation: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable> {
        let request = OutboundServiceRequest::continuing(ServiceId::notifications(), correlation);
        let response = self
            .client
            .exchange_json(&request, method, &path, body)
            .await
            .map_err(|e: InternalCallError| PushUnavailable(e.to_string()))?;
        match response.status() {
            204 => Ok(PushReply::Ok(Value::Null)),
            200..=299 => response
                .json::<Value>()
                .map(PushReply::Ok)
                .map_err(|_| PushUnavailable("malformed push provider response".into())),
            status @ (400 | 403 | 404 | 409 | 422) => Ok(PushReply::Refused {
                status,
                code: response
                    .error_code()
                    .unwrap_or_else(|| "REFUSED".into())
                    .chars()
                    .take(64)
                    .collect(),
            }),
            status => Err(PushUnavailable(format!("HTTP {status}"))),
        }
    }
}

#[async_trait::async_trait]
impl PushProviderPort for HttpPushProviders {
    async fn put(
        &self,
        scope: &PushScope,
        provider: &str,
        body: &Value,
        correlation: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable> {
        self.call(
            axum::http::Method::PUT,
            scope.path(provider),
            Some(body),
            correlation,
        )
        .await
    }

    async fn delete(
        &self,
        scope: &PushScope,
        provider: &str,
        correlation: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable> {
        self.call(
            axum::http::Method::DELETE,
            scope.path(provider),
            None,
            correlation,
        )
        .await
    }

    async fn verify(
        &self,
        scope: &PushScope,
        provider: &str,
        correlation: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable> {
        self.call(
            axum::http::Method::POST,
            format!("{}/verify", scope.path(provider)),
            None,
            correlation,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SealingKey {
        let (public, _) =
            cf_notification_client::seal::generate_keypair("notif-hpke-2026-11").unwrap();
        SealingKey::parse(&public).unwrap()
    }

    fn scope() -> PushScope {
        PushScope {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn short_secrets_seal_to_one_part_and_long_ones_to_bounded_parts() {
        let k = key();
        let s = scope();
        let p8 = b"-----BEGIN PRIVATE KEY-----\nMIGTAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBHkwdwIBAQQg\n-----END PRIVATE KEY-----\n";
        let sealed: Value = serde_json::from_str(&seal_secret(&k, &s, "APNS", p8).unwrap()).unwrap();
        assert_eq!(sealed["v"], 1);
        assert_eq!(sealed["parts"].as_array().unwrap().len(), 1);
        assert_eq!(sealed["parts"][0]["kid"], "notif-hpke-2026-11");
        let text = sealed.to_string();
        assert!(!text.contains("BEGIN PRIVATE KEY"));
        let json = vec![b'x'; 3000];
        let sealed: Value =
            serde_json::from_str(&seal_secret(&k, &s, "FCM", &json).unwrap()).unwrap();
        // 3000 bytes -> 4000 base64url characters -> 8 parts of 500.
        assert_eq!(sealed["parts"].as_array().unwrap().len(), 8);
        assert!(seal_secret(&k, &s, "FCM", &[]).is_err());
        assert!(seal_secret(&k, &s, "FCM", &vec![b'x'; MAX_SECRET_BYTES + 1]).is_err());
        assert_eq!(seal_event_type(0, 8), "push-provider.secret.0-of-8");
        assert!(s.seal_event_id("APNS_VOIP").ends_with("/APNS_VOIP"));
    }
}
