//! Calls from the bridge to the owning CF Meet services (design §4).
//!
//! * The bridge signs with its OWN `cf-service+jwt` (`cfmeet.admin-bridge`, one audience,
//!   short-lived); the HQ staff token is never passed on.
//! * Every call carries `X-CF-Operator: hq:<sub>`; owners accept it only from the bridge.
//! * Reads only in A2. A 401 / 403 from an owner is OUR misconfiguration (missing grant)
//!   and never becomes data: it is logged and answered `OWNER_UNAVAILABLE`.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient, InternalTargets},
    OutboundServiceRequest, ServiceId,
};
use serde_json::Value;

use crate::contract::model::Owner;
use crate::errors::ErrorCode;

pub fn service_id(owner: Owner) -> Option<ServiceId> {
    Some(match owner {
        Owner::MeetingService => ServiceId::meeting_service(),
        Owner::Messaging => ServiceId::messaging(),
        Owner::Calling => ServiceId::calling(),
        Owner::Communications => ServiceId::communications(),
        Owner::Organizations => ServiceId::organizations(),
        Owner::DeveloperPlatform => ServiceId::developer_platform(),
        Owner::PlatformAccess => ServiceId::platform_access(),
        Owner::Notifications => ServiceId::notifications(),
        Owner::Numbering | Owner::AdminBridge => return None,
    })
}

/// Environment variable of an owner's internal base URL (`CF_<OWNER>_BASE_URL`).
pub fn base_url_var(owner: Owner) -> String {
    format!(
        "CF_{}_BASE_URL",
        owner.as_str().replace('-', "_").to_ascii_uppercase()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OwnerError {
    #[error("the owner has no such resource")]
    NotFound,
    #[error("the owner is unavailable or refused the bridge")]
    Unavailable,
    #[error("the owner answered outside its contract")]
    Contract,
}

impl From<OwnerError> for ErrorCode {
    fn from(e: OwnerError) -> Self {
        match e {
            OwnerError::NotFound => ErrorCode::ResourceNotFound,
            OwnerError::Unavailable => ErrorCode::OwnerUnavailable,
            OwnerError::Contract => ErrorCode::Internal,
        }
    }
}

/// One read from one owner.
#[async_trait]
pub trait OwnerPort: Send + Sync + 'static {
    /// `GET path?query` on `owner`, attributed to `operator` (`hq:<sub>`).
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        operator: &str,
    ) -> Result<Value, OwnerError>;
}

/// No owner is configured: every owner read is unavailable (honest, never empty data).
pub struct NoOwners;

#[async_trait]
impl OwnerPort for NoOwners {
    async fn read(
        &self,
        _: Owner,
        _: &str,
        _: &[(&str, String)],
        _: &str,
    ) -> Result<Value, OwnerError> {
        Err(OwnerError::Unavailable)
    }
}

/// Signed HTTP calls through the shared `InternalServiceClient` (no redirects, no proxy,
/// bounded timeouts and response size, `/internal/v1/` paths only).
pub struct HttpOwners {
    client: Arc<InternalServiceClient>,
    configured: BTreeMap<Owner, ServiceId>,
}

impl HttpOwners {
    pub fn new(client: Arc<InternalServiceClient>, targets: &InternalTargets) -> Self {
        let configured = Owner::ALL
            .iter()
            .filter_map(|o| {
                service_id(*o)
                    .filter(|s| targets.contains(s))
                    .map(|s| (*o, s))
            })
            .collect();
        HttpOwners { client, configured }
    }
}

#[async_trait]
impl OwnerPort for HttpOwners {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        operator: &str,
    ) -> Result<Value, OwnerError> {
        let Some(target) = self.configured.get(&owner) else {
            return Err(OwnerError::Unavailable);
        };
        let request = OutboundServiceRequest::new_operation(target.clone());
        let q: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let headers = [("x-cf-operator", operator)];
        let result = self
            .client
            .exchange_json_query_with_headers::<()>(
                &request,
                http::Method::GET,
                path,
                &q,
                &headers,
                None,
            )
            .await;
        let response = match result {
            Ok(r) => r,
            Err(InternalCallError::Status { status: 404, .. }) => return Err(OwnerError::NotFound),
            Err(InternalCallError::Status {
                status: s @ (401 | 403),
                code,
                ..
            }) => {
                tracing::error!(
                    owner = owner.as_str(),
                    status = s,
                    code = code.as_deref().unwrap_or("-"),
                    "owner refused the bridge: check its trust bundle grant"
                );
                return Err(OwnerError::Unavailable);
            }
            Err(InternalCallError::Status { status, code, .. }) if (400..500).contains(&status) => {
                tracing::error!(
                    owner = owner.as_str(),
                    status,
                    code = code.as_deref().unwrap_or("-"),
                    "owner rejected a bridge read as invalid"
                );
                return Err(OwnerError::Contract);
            }
            Err(
                InternalCallError::InvalidPath
                | InternalCallError::InvalidHeader
                | InternalCallError::Encoding,
            ) => return Err(OwnerError::Contract),
            Err(e) => {
                tracing::warn!(owner = owner.as_str(), error = %e, "owner read failed");
                return Err(OwnerError::Unavailable);
            }
        };
        if !(200..300).contains(&response.status()) {
            return Err(OwnerError::Unavailable);
        }
        response.json::<Value>().map_err(|_| OwnerError::Contract)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OwnerSetupError {
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

/// Builds the signed owner client from configuration (key file loaded here, once).
pub fn http_owners(
    s: &crate::config::OutboundSettings,
    environment: cf_service_auth::RuntimeEnvironment,
) -> Result<HttpOwners, OwnerSetupError> {
    use cf_service_auth::{KeyId, ServiceSigningKey, ServiceTokenSigner, SystemClock, TokenTtl};
    let bad = |var: &'static str, reason: String| OwnerSetupError::Invalid { var, reason };
    let kid = KeyId::parse(&s.key_id).map_err(|e| bad("CF_SERVICE_AUTH_KEY_ID", e.to_string()))?;
    let key = ServiceSigningKey::from_pkcs8_pem_file(kid, &s.private_key_file)
        .map_err(|e| bad("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", e.to_string()))?;
    let ttl = TokenTtl::seconds(s.token_ttl_seconds)
        .map_err(|e| bad("CF_SERVICE_TOKEN_TTL_SECONDS", e.to_string()))?;
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::admin_bridge(),
        environment,
        key,
        ttl,
        Arc::new(SystemClock),
    ));
    let mut targets = InternalTargets::new(environment);
    for (owner, url) in &s.base_urls {
        if let Some(id) = service_id(*owner) {
            let var: &'static str = Box::leak(base_url_var(*owner).into_boxed_str());
            targets
                .insert(id, url)
                .map_err(|e| bad(var, e.to_string()))?;
        }
    }
    let client = InternalServiceClient::new(
        signer,
        targets.clone(),
        std::time::Duration::from_millis(s.timeout_ms),
    )
    .map_err(|e| bad("CF_INTERNAL_HTTP_TIMEOUT_MS", e.to_string()))?;
    Ok(HttpOwners::new(Arc::new(client), &targets))
}
