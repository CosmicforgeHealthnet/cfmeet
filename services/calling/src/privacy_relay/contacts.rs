//! Contact Vault delegated access (Part D §31): Calling MINTS short-lived Contact Vault
//! access tokens; it never receives a telephone number.
//!
//! `POST /v1/privacy-relay/contact-access-grants` authenticates the customer's ordinary
//! platform credential, requires every requested `privacy.contacts.*` scope on that
//! credential, and returns a token signed with Calling's DEDICATED contact delegation
//! key (never its service-auth key). The customer then sends the raw number straight to
//! Privacy Relay's Contact Vault with `X-CF-Contact-Authorization: Bearer <token>`.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `CONTACT_DELEGATION_KEY_ID` | with the key | `kid` of the contact delegation key |
//! | `CONTACT_DELEGATION_PRIVATE_KEY_FILE` | with the kid | PKCS#8 PEM Ed25519 key FILE (distinct from the service key) |
//! | `CONTACT_DELEGATION_TTL_SECONDS` | no | 10–300, default 120 |
//!
//! Without them the grant endpoint answers 503 (no contact access is possible).

use std::path::PathBuf;

use axum::{
    extract::{rejection::JsonRejection, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use cf_service_auth::{
    ContactDelegationSigner, ContactScope, KeyId, PublicJwk, RuntimeEnvironment, ServiceId,
    ServiceSigningKey, SharedClock,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::{
    error::CallingError, internal_auth::delegation::delegated_context,
    platform::context::PlatformAuthContext, state::AppState,
};

#[derive(Debug, thiserror::Error)]
pub enum ContactDelegationConfigError {
    #[error("{0} is invalid: {1}")]
    Invalid(&'static str, String),
}

fn invalid(var: &'static str, reason: impl Into<String>) -> ContactDelegationConfigError {
    ContactDelegationConfigError::Invalid(var, reason.into())
}

/// Contact delegation key settings.
#[derive(Debug, Clone, Default)]
pub struct ContactDelegationSettings {
    pub key: Option<(KeyId, PathBuf)>,
    pub ttl_seconds: u64,
}

impl ContactDelegationSettings {
    pub fn from_lookup(
        get: &impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ContactDelegationConfigError> {
        let value = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        if value("CONTACT_DELEGATION_PRIVATE_KEY").is_some() {
            return Err(invalid(
                "CONTACT_DELEGATION_PRIVATE_KEY",
                "inline private keys are not accepted; mount the key as a secret file",
            ));
        }
        let key = match (value("CONTACT_DELEGATION_KEY_ID"), value("CONTACT_DELEGATION_PRIVATE_KEY_FILE")) {
            (None, None) => None,
            (Some(kid), Some(file)) => Some((
                KeyId::parse(&kid).map_err(|e| invalid("CONTACT_DELEGATION_KEY_ID", e.to_string()))?,
                PathBuf::from(file),
            )),
            _ => {
                return Err(invalid(
                    "CONTACT_DELEGATION_PRIVATE_KEY_FILE",
                    "CONTACT_DELEGATION_KEY_ID and CONTACT_DELEGATION_PRIVATE_KEY_FILE are configured together",
                ))
            }
        };
        let ttl_seconds = match value("CONTACT_DELEGATION_TTL_SECONDS") {
            None => cf_service_auth::contact_delegation::DEFAULT_CONTACT_TTL_SECONDS,
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|s| (10..=300).contains(s))
                .ok_or_else(|| {
                    invalid(
                        "CONTACT_DELEGATION_TTL_SECONDS",
                        "must be an integer between 10 and 300",
                    )
                })?,
        };
        Ok(Self { key, ttl_seconds })
    }

    /// Loads the key (fatal on any failure). The key must differ from the service-auth
    /// key in both id and material: one key never signs two kinds of credential.
    pub fn load(
        &self,
        runtime_environment: RuntimeEnvironment,
        service_key: &PublicJwk,
        clock: SharedClock,
    ) -> Result<Option<ContactDelegationSigner>, ContactDelegationConfigError> {
        let Some((kid, file)) = &self.key else {
            return Ok(None);
        };
        let key = ServiceSigningKey::from_pkcs8_pem_file(kid.clone(), file)
            .map_err(|e| invalid("CONTACT_DELEGATION_PRIVATE_KEY_FILE", e.to_string()))?;
        if &service_key.kid == kid || key.public_jwk().x == service_key.x {
            return Err(invalid(
                "CONTACT_DELEGATION_PRIVATE_KEY_FILE",
                "the contact delegation key must be a different key from the service-auth key",
            ));
        }
        ContactDelegationSigner::new(
            ServiceId::calling(),
            runtime_environment,
            key,
            self.ttl_seconds,
            clock,
        )
        .map(Some)
        .map_err(|e| invalid("CONTACT_DELEGATION_TTL_SECONDS", e.to_string()))
    }
}

/// `POST /v1/privacy-relay/contact-access-grants` body: the requested scopes only (no
/// telephone number, no scope / tenant fields).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContactAccessGrantRequest {
    scopes: Vec<String>,
}

fn iso(unix: i64) -> String {
    DateTime::<Utc>::from_timestamp(unix, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub async fn create_contact_access_grant(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<ContactAccessGrantRequest>, JsonRejection>,
) -> Result<Response, CallingError> {
    let Json(request) =
        body.map_err(|_| CallingError::Validation("The request body is invalid."))?;
    if request.scopes.is_empty() || request.scopes.len() > ContactScope::ALL.len() {
        return Err(CallingError::Validation(
            "scopes must name 1 to 3 Contact Vault scopes.",
        ));
    }
    let mut scopes = Vec::new();
    for name in &request.scopes {
        let scope = ContactScope::parse(name).ok_or(CallingError::Validation(
            "scopes may only name privacy.contacts.read, .write or .verify.",
        ))?;
        if scopes.contains(&scope) {
            return Err(CallingError::Validation("scopes must be unique."));
        }
        // The credential itself must hold every delegated scope.
        ctx.require_scope(scope.as_str())?;
        scopes.push(scope);
    }
    let Some(signer) = state.contact_delegation.as_ref() else {
        return Err(CallingError::ServiceUnavailable);
    };
    let delegated = delegated_context(&ctx, None).map_err(|_| CallingError::Internal)?;
    let token = signer
        .sign(&ServiceId::privacy_relay(), &delegated, &scopes)
        .map_err(|_| CallingError::Internal)?;
    tracing::info!(
        grant_id = token.token_id().as_str(),
        scopes = ?token.scopes().iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        "contact vault access grant issued"
    );
    let mut response = (
        StatusCode::CREATED,
        Json(json!({
            "grant": {
                "grantId": token.token_id().as_str(),
                "accessToken": token.expose_for_delivery(),
                "tokenType": "Bearer",
                "authorizationHeader": "X-CF-Contact-Authorization",
                "audience": ServiceId::privacy_relay().as_str(),
                "scopes": token.scopes().iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                "expiresAt": iso(token.expires_at()),
            }
        })),
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    Ok(response)
}
