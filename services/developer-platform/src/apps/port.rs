//! The authorities the CF Meet Apps features call (integration contract §2, §3).
//!
//! * [`PlatformOAuthPort`] — Platform Access, the token authority: the OAuth client profile
//!   of an app, its client secret, its grants (`platform-oauth.manage`), the token /
//!   revocation / introspection backend (`platform-oauth.token`) and the identity change
//!   feed whose `APP_GRANT` entries the install projection follows
//!   (`platform-identity.read`). The Developer Platform verifies no client secret and no
//!   token itself: every answer comes from Platform Access.
//! * [`AccountProfiles`] — the Meeting Service, the CF Meet account authority:
//!   `GET /internal/v1/oauth/userinfo` (`meeting-oauth-userinfo.read`).
//!
//! Unreachable means refused (fail closed).

use std::sync::Arc;

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, OutboundServiceRequest, ServiceId,
};
use serde_json::Value;
use uuid::Uuid;

use crate::platform_access::AuthorityUnavailable;

/// A Platform Access answer: 2xx with its body (`null` for an empty body), or a 4xx
/// refusal with its `error.code` (the RFC 6749 lower-case codes of the token backend
/// included).
#[derive(Debug, Clone, PartialEq)]
pub enum OAuthReply {
    Ok { status: u16, body: Value },
    Refused { status: u16, code: String },
}

#[async_trait::async_trait]
pub trait PlatformOAuthPort: Send + Sync {
    /// `PUT /internal/v1/oauth/clients/{applicationId}` (create or replace the profile).
    async fn put_client(
        &self,
        app_id: Uuid,
        body: &Value,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable>;

    /// `POST /internal/v1/oauth/clients/{applicationId}/secret` (`201 {clientSecret,
    /// secretId, issuedAt}`; the previous secret keeps a 24 h overlap).
    async fn issue_secret(
        &self,
        app_id: Uuid,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable>;

    /// `GET /internal/v1/oauth/clients/{applicationId}/grants?cursor`.
    async fn client_grants(
        &self,
        app_id: Uuid,
        cursor: Option<&str>,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable>;

    /// `POST /internal/v1/oauth/token`.
    async fn token(
        &self,
        body: &Value,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable>;

    /// `POST /internal/v1/oauth/revoke`.
    async fn revoke(
        &self,
        body: &Value,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable>;

    /// `POST /internal/v1/oauth/introspect`.
    async fn introspect(
        &self,
        access_token: &str,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable>;

    /// `POST /internal/v1/identity/changes` (`{after, limit}` → `{changes, nextAfter,
    /// hasMore}`).
    async fn identity_changes(
        &self,
        after: i64,
        limit: u32,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable>;
}

/// The Meeting Service's CF Meet account profile for `/v1/oauth/userinfo`.
#[async_trait::async_trait]
pub trait AccountProfiles: Send + Sync {
    /// `{displayName, avatarUrl, email?}` of an ACTIVE account (`None`: unknown or not
    /// active). `scopes` are the grant's OAuth scopes (the email only with
    /// `profile:email`).
    async fn userinfo(
        &self,
        account_id: Uuid,
        scopes: &[String],
        correlation: &CorrelationId,
    ) -> Result<Option<Value>, AuthorityUnavailable>;
}

fn unavailable(e: InternalCallError) -> AuthorityUnavailable {
    AuthorityUnavailable(e.to_string())
}

/// The `error.code` of a refusal: upper-case Developer Platform / Platform Access codes or
/// the RFC 6749 lower-case ones (letters, digits and `_`, at most 64).
pub fn refusal_code(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/code")
                .or_else(|| v.get("error").filter(|e| e.is_string()))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|c| {
            !c.is_empty()
                && c.len() <= 64
                && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .unwrap_or_else(|| "REFUSED".into())
}

/// Platform Access over authenticated internal HTTP.
pub struct HttpPlatformOAuth {
    client: Arc<InternalServiceClient>,
}

impl HttpPlatformOAuth {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }

    async fn call(
        &self,
        method: axum::http::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        let request = OutboundServiceRequest::continuing(ServiceId::platform_access(), correlation);
        let response = self
            .client
            .exchange_json_query(&request, method, path, query, body)
            .await
            .map_err(unavailable)?;
        match response.status() {
            status @ 200..=299 => {
                let body = if response.body().is_empty() {
                    Value::Null
                } else {
                    response.json::<Value>().map_err(|_| {
                        AuthorityUnavailable("malformed platform access response".into())
                    })?
                };
                Ok(OAuthReply::Ok { status, body })
            }
            status @ 400..=499 => Ok(OAuthReply::Refused {
                status,
                code: refusal_code(response.body()),
            }),
            status => Err(AuthorityUnavailable(format!("HTTP {status}"))),
        }
    }
}

#[async_trait::async_trait]
impl PlatformOAuthPort for HttpPlatformOAuth {
    async fn put_client(
        &self,
        app_id: Uuid,
        body: &Value,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.call(
            axum::http::Method::PUT,
            &format!("/internal/v1/oauth/clients/{app_id}"),
            &[],
            Some(body),
            correlation,
        )
        .await
    }

    async fn issue_secret(
        &self,
        app_id: Uuid,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.call(
            axum::http::Method::POST,
            &format!("/internal/v1/oauth/clients/{app_id}/secret"),
            &[],
            Some(&serde_json::json!({})),
            correlation,
        )
        .await
    }

    async fn client_grants(
        &self,
        app_id: Uuid,
        cursor: Option<&str>,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        let query: Vec<(&str, &str)> = cursor.map(|c| vec![("cursor", c)]).unwrap_or_default();
        self.call(
            axum::http::Method::GET,
            &format!("/internal/v1/oauth/clients/{app_id}/grants"),
            &query,
            None,
            correlation,
        )
        .await
    }

    async fn token(
        &self,
        body: &Value,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.call(
            axum::http::Method::POST,
            "/internal/v1/oauth/token",
            &[],
            Some(body),
            correlation,
        )
        .await
    }

    async fn revoke(
        &self,
        body: &Value,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.call(
            axum::http::Method::POST,
            "/internal/v1/oauth/revoke",
            &[],
            Some(body),
            correlation,
        )
        .await
    }

    async fn introspect(
        &self,
        access_token: &str,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.call(
            axum::http::Method::POST,
            "/internal/v1/oauth/introspect",
            &[],
            Some(&serde_json::json!({ "token": access_token })),
            correlation,
        )
        .await
    }

    async fn identity_changes(
        &self,
        after: i64,
        limit: u32,
        correlation: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.call(
            axum::http::Method::POST,
            "/internal/v1/identity/changes",
            &[],
            Some(&serde_json::json!({ "after": after, "limit": limit })),
            correlation,
        )
        .await
    }
}

/// The Meeting Service over authenticated internal HTTP.
pub struct HttpAccountProfiles {
    client: Arc<InternalServiceClient>,
}

impl HttpAccountProfiles {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl AccountProfiles for HttpAccountProfiles {
    async fn userinfo(
        &self,
        account_id: Uuid,
        scopes: &[String],
        correlation: &CorrelationId,
    ) -> Result<Option<Value>, AuthorityUnavailable> {
        let request = OutboundServiceRequest::continuing(ServiceId::meeting_service(), correlation);
        let account = account_id.to_string();
        let scopes = scopes.join(" ");
        let response = self
            .client
            .exchange_json_query::<Value>(
                &request,
                axum::http::Method::GET,
                "/internal/v1/oauth/userinfo",
                &[("accountId", account.as_str()), ("scopes", scopes.as_str())],
                None,
            )
            .await
            .map_err(unavailable)?;
        match response.status() {
            200 => response
                .json::<Value>()
                .map(Some)
                .map_err(|_| AuthorityUnavailable("malformed userinfo response".into())),
            404 | 410 => Ok(None),
            status => Err(AuthorityUnavailable(format!("HTTP {status}"))),
        }
    }
}

/// No Platform Access configured: everything fails closed.
pub struct UnconfiguredPlatformOAuth;

fn not_configured() -> AuthorityUnavailable {
    AuthorityUnavailable("no token authority is configured".into())
}

#[async_trait::async_trait]
impl PlatformOAuthPort for UnconfiguredPlatformOAuth {
    async fn put_client(
        &self,
        _: Uuid,
        _: &Value,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        Err(not_configured())
    }

    async fn issue_secret(
        &self,
        _: Uuid,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        Err(not_configured())
    }

    async fn client_grants(
        &self,
        _: Uuid,
        _: Option<&str>,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        Err(not_configured())
    }

    async fn token(&self, _: &Value, _: &CorrelationId) -> Result<OAuthReply, AuthorityUnavailable> {
        Err(not_configured())
    }

    async fn revoke(
        &self,
        _: &Value,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        Err(not_configured())
    }

    async fn introspect(
        &self,
        _: &str,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        Err(not_configured())
    }

    async fn identity_changes(
        &self,
        _: i64,
        _: u32,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        Err(not_configured())
    }
}

/// No Meeting Service configured: userinfo fails closed.
pub struct UnconfiguredAccountProfiles;

#[async_trait::async_trait]
impl AccountProfiles for UnconfiguredAccountProfiles {
    async fn userinfo(
        &self,
        _: Uuid,
        _: &[String],
        _: &CorrelationId,
    ) -> Result<Option<Value>, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no account authority is configured".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_codes_keep_rfc_and_platform_codes_only() {
        assert_eq!(
            refusal_code(br#"{"error":{"code":"invalid_grant"}}"#),
            "invalid_grant"
        );
        assert_eq!(
            refusal_code(br#"{"error":{"code":"VALIDATION_ERROR"}}"#),
            "VALIDATION_ERROR"
        );
        assert_eq!(refusal_code(br#"{"error":"invalid_client"}"#), "invalid_client");
        assert_eq!(
            refusal_code(br#"{"error":{"code":"bad code <script>"}}"#),
            "REFUSED"
        );
        assert_eq!(refusal_code(b"not json"), "REFUSED");
    }
}
