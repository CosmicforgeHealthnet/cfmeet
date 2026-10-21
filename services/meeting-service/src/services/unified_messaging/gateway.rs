//! The authenticated boundary to Platform Access and Messaging (Part V H §98, §104).
//!
//! Every call carries a short-lived Ed25519 service token of `cfmeet.meeting-service`
//! with CF Meet's own FIRST_PARTY delegated scope signed into it, and — when the call acts
//! for an end user — that user's communication principal as the signed initiating
//! principal. There is no API credential, no shared secret and no network-location trust;
//! nothing here is ever sent to a browser or mobile client.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient, InternalTargets},
    DelegatedPlatformContext, DelegatedPrincipalKind, OutboundServiceRequest, ServiceId,
    ServiceSigningKey, ServiceTokenSigner, SystemClock, TokenTtl,
};
use serde_json::Value;
use uuid::Uuid;

use super::config::UnifiedMessagingConfig;

pub type PrincipalKind = DelegatedPrincipalKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    PlatformAccess,
    Messaging,
}

/// The end user a call acts for (signed into the service token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acting {
    pub principal_id: Uuid,
    pub kind: PrincipalKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayError {
    /// The peer is unreachable, timed out or answered with an unusable response.
    Unavailable(&'static str),
    /// The peer refused the request with its typed error envelope.
    Refused {
        status: u16,
        code: String,
        reason: Option<String>,
    },
}

impl GatewayError {
    pub fn code(&self) -> Option<&str> {
        match self {
            GatewayError::Refused { code, .. } => Some(code),
            GatewayError::Unavailable(_) => None,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            GatewayError::Refused { reason, .. } => reason.as_deref(),
            GatewayError::Unavailable(_) => None,
        }
    }

    fn from_response(status: u16, body: &Value) -> Self {
        let code = body["error"]["code"]
            .as_str()
            .unwrap_or("UNKNOWN")
            .to_string();
        let reason = body["error"]["reason"].as_str().map(str::to_string);
        if status >= 500 {
            // Server-side failure of the peer: retryable, never a business answer.
            return GatewayError::Unavailable("peer server error");
        }
        GatewayError::Refused {
            status,
            code,
            reason,
        }
    }
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayError::Unavailable(why) => write!(f, "unavailable: {why}"),
            GatewayError::Refused {
                status,
                code,
                reason,
            } => write!(
                f,
                "refused {status} {code} {}",
                reason.as_deref().unwrap_or("")
            ),
        }
    }
}

#[async_trait]
pub trait MessagingGateway: Send + Sync {
    /// CF Meet's communication environment (the scope every principal lives in).
    fn environment_id(&self) -> Uuid;

    /// One signed call; returns the peer's status and JSON body for EVERY status the
    /// peer answered (transport failures are [`GatewayError::Unavailable`]).
    async fn call_raw(
        &self,
        peer: Peer,
        acting: Option<Acting>,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value), GatewayError>;

    /// [`call_raw`](Self::call_raw) that also forwards the client's allowlisted request
    /// headers (`Idempotency-Key`). Adapters without header support relay without them.
    #[allow(clippy::too_many_arguments)]
    async fn call_raw_with_headers(
        &self,
        peer: Peer,
        acting: Option<Acting>,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value), GatewayError> {
        let _ = headers;
        self.call_raw(peer, acting, method, path, query, body).await
    }

    /// CF Meet's communication scope (tenant, application, environment), when known.
    fn scope_ids(&self) -> Option<(Uuid, Uuid, Uuid)> {
        None
    }

    /// [`call_raw`](Self::call_raw) that maps non-2xx answers to [`GatewayError`].
    async fn call(
        &self,
        peer: Peer,
        acting: Option<Acting>,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, GatewayError> {
        let (status, value) = self
            .call_raw(peer, acting, method, path, query, body)
            .await?;
        if (200..300).contains(&status) {
            Ok(value)
        } else {
            Err(GatewayError::from_response(status, &value))
        }
    }
}

pub type SharedGateway = Arc<dyn MessagingGateway>;

/// Production gateway: `cf-service-auth` signed internal HTTP.
pub struct ServiceAuthGateway {
    client: InternalServiceClient,
    scope: DelegatedPlatformContext,
    environment_id: Uuid,
    ids: (Uuid, Uuid, Uuid),
}

impl ServiceAuthGateway {
    pub fn new(config: &UnifiedMessagingConfig) -> Result<Self, String> {
        let key = ServiceSigningKey::from_pkcs8_pem_file(config.key_id.clone(), &config.key_file)
            .map_err(|_| {
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE is not a readable PKCS#8 Ed25519 key"
        })?;
        let ttl = TokenTtl::seconds(config.token_ttl_seconds)
            .map_err(|_| "CF_SERVICE_TOKEN_TTL_SECONDS is out of range")?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            config.runtime,
            key,
            ttl,
            Arc::new(SystemClock),
        ));
        let mut targets = InternalTargets::new(config.runtime);
        targets
            .insert(ServiceId::platform_access(), &config.platform_access_url)
            .map_err(|e| format!("CF_PLATFORM_ACCESS_BASE_URL: {e}"))?;
        targets
            .insert(ServiceId::messaging(), &config.messaging_url)
            .map_err(|e| format!("CF_MESSAGING_BASE_URL: {e}"))?;
        let client = InternalServiceClient::new(signer, targets, config.timeout)
            .map_err(|e| format!("internal client: {e}"))?;
        let s = config.scope;
        Ok(Self {
            client,
            scope: DelegatedPlatformContext::new(
                s.tenant_id,
                s.application_id,
                s.environment_id,
                s.kind,
                s.service_account_id,
            ),
            environment_id: s.environment_id,
            ids: (s.tenant_id, s.application_id, s.environment_id),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn exchange(
        &self,
        peer: Peer,
        acting: Option<Acting>,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value), GatewayError> {
        let audience = match peer {
            Peer::PlatformAccess => ServiceId::platform_access(),
            Peer::Messaging => ServiceId::messaging(),
        };
        let scope = match acting {
            Some(a) => self
                .scope
                .clone()
                .with_initiating_principal(a.principal_id, a.kind),
            None => self.scope.clone(),
        };
        let request = OutboundServiceRequest::new_operation(audience).with_delegated_context(scope);
        let started = std::time::Instant::now();
        let result = if headers.is_empty() {
            self.client
                .exchange_json_query(&request, method, path, query, body)
                .await
        } else {
            self.client
                .exchange_json_query_with_headers(&request, method, path, query, headers, body)
                .await
        };
        let peer_label = match peer {
            Peer::PlatformAccess => "platform-access",
            Peer::Messaging => "messaging",
        };
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                let why = match e {
                    InternalCallError::Timeout { .. } => "timeout",
                    InternalCallError::Connect { .. } => "connect",
                    InternalCallError::ResponseTooLarge { .. } => "response too large",
                    InternalCallError::InvalidHeader => "invalid forwarded header",
                    _ => "transport",
                };
                crate::services::ops_metrics::unified_messaging_call(peer_label, why);
                tracing::warn!(
                    peer = peer_label,
                    error = why,
                    "unified messaging call failed"
                );
                return Err(GatewayError::Unavailable(why));
            }
        };
        let status = response.status();
        crate::services::ops_metrics::unified_messaging_call(
            peer_label,
            if (200..300).contains(&status) {
                "ok"
            } else if status >= 500 {
                "server_error"
            } else {
                "refused"
            },
        );
        tracing::debug!(
            peer = peer_label,
            status,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "unified messaging call"
        );
        let value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body())
                .map_err(|_| GatewayError::Unavailable("invalid response body"))?
        };
        Ok((status, value))
    }
}

#[async_trait]
impl MessagingGateway for ServiceAuthGateway {
    fn environment_id(&self) -> Uuid {
        self.environment_id
    }

    fn scope_ids(&self) -> Option<(Uuid, Uuid, Uuid)> {
        Some(self.ids)
    }

    async fn call_raw(
        &self,
        peer: Peer,
        acting: Option<Acting>,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value), GatewayError> {
        self.exchange(peer, acting, method, path, query, &[], body)
            .await
    }

    async fn call_raw_with_headers(
        &self,
        peer: Peer,
        acting: Option<Acting>,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value), GatewayError> {
        self.exchange(peer, acting, method, path, query, headers, body)
            .await
    }
}
