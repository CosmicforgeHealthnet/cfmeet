//! Platform Access, the OAuth token authority (ADR 0064, integrate contract §2
//! "Authorization", permission `platform-oauth.authorize`, Meeting Service only):
//!
//! * `POST /internal/v1/oauth/authorization-requests/validate`
//! * `POST /internal/v1/oauth/authorizations`
//! * `GET /internal/v1/oauth/grants?accountId=…` / `?organizationId=…`
//! * `DELETE /internal/v1/oauth/grants/{grantId}`
//!
//! The port returns every HTTP answer as-is ([`PaAnswer`]); the account authority
//! ([`super::service`]) decides. Unreachable is [`AuthorityUnavailable`] (503, fail
//! closed). The signed-in account id is always the server-resolved one.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use serde_json::Value;
use uuid::Uuid;

use crate::services::platform::authority::AuthorityUnavailable;

pub const VALIDATE_PATH: &str = "/internal/v1/oauth/authorization-requests/validate";
pub const AUTHORIZATIONS_PATH: &str = "/internal/v1/oauth/authorizations";
pub const GRANTS_PATH: &str = "/internal/v1/oauth/grants";

/// One Platform Access answer (status + JSON body, `Null` when empty / not JSON).
#[derive(Debug, Clone, PartialEq)]
pub struct PaAnswer {
    pub status: u16,
    pub body: Value,
}

impl PaAnswer {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `error.code` of a refusal (bounded, `[A-Z0-9_]`).
    pub fn error_code(&self) -> Option<String> {
        self.body
            .pointer("/error/code")
            .and_then(Value::as_str)
            .map(|c| {
                c.chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .take(64)
                    .collect()
            })
    }
}

/// Whose grants to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantOwner {
    Account(Uuid),
    Organization(Uuid),
}

#[async_trait]
pub trait OauthAuthority: Send + Sync {
    async fn validate(&self, request: &Value) -> Result<PaAnswer, AuthorityUnavailable>;
    async fn authorize(&self, request: &Value) -> Result<PaAnswer, AuthorityUnavailable>;
    async fn grants(&self, owner: GrantOwner) -> Result<PaAnswer, AuthorityUnavailable>;
    async fn revoke(&self, grant_id: Uuid) -> Result<PaAnswer, AuthorityUnavailable>;
}

pub type SharedOauthAuthority = Arc<dyn OauthAuthority>;

/// Platform Access over authenticated internal HTTP.
pub struct PlatformAccessOauth {
    client: Arc<InternalServiceClient>,
}

impl PlatformAccessOauth {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<PaAnswer, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(ServiceId::platform_access());
        let response = self
            .client
            .exchange_json_query(&request, method, path, query, body)
            .await
            .map_err(|e: InternalCallError| AuthorityUnavailable(e.to_string()))?;
        let status = response.status();
        if status >= 500 || status == 429 {
            return Err(AuthorityUnavailable(format!("HTTP {status}")));
        }
        let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        Ok(PaAnswer { status, body })
    }
}

#[async_trait]
impl OauthAuthority for PlatformAccessOauth {
    async fn validate(&self, request: &Value) -> Result<PaAnswer, AuthorityUnavailable> {
        self.call(Method::POST, VALIDATE_PATH, &[], Some(request))
            .await
    }

    async fn authorize(&self, request: &Value) -> Result<PaAnswer, AuthorityUnavailable> {
        self.call(Method::POST, AUTHORIZATIONS_PATH, &[], Some(request))
            .await
    }

    async fn grants(&self, owner: GrantOwner) -> Result<PaAnswer, AuthorityUnavailable> {
        let (key, id) = match owner {
            GrantOwner::Account(id) => ("accountId", id),
            GrantOwner::Organization(id) => ("organizationId", id),
        };
        let id = id.to_string();
        self.call(Method::GET, GRANTS_PATH, &[(key, id.as_str())], None)
            .await
    }

    async fn revoke(&self, grant_id: Uuid) -> Result<PaAnswer, AuthorityUnavailable> {
        self.call(
            Method::DELETE,
            &format!("{GRANTS_PATH}/{grant_id}"),
            &[],
            None,
        )
        .await
    }
}

/// An app's meeting surface (ADR 0065: the iframe URL and its allowed origins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingSurface {
    pub url: String,
    pub origins: Vec<String>,
}

/// One grant as Platform Access lists it (parsed defensively; unknown fields ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: Uuid,
    pub app_id: Uuid,
    /// `USER` or `ORGANIZATION`.
    pub target: String,
    pub organization_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
    pub scopes: Vec<String>,
    /// `ACTIVE` unless Platform Access says otherwise.
    pub status: String,
    pub app_name: Option<String>,
    pub developer_name: Option<String>,
    pub logo_url: Option<String>,
    pub meeting_surface: Option<MeetingSurface>,
    /// The pairwise subject of the granting user for this app, when listed.
    pub sub: Option<String>,
    pub created_at: Option<String>,
}

fn uuid_at(v: &Value, keys: &[&str]) -> Option<Uuid> {
    keys.iter()
        .filter_map(|k| v.get(*k).and_then(Value::as_str))
        .find_map(|s| Uuid::parse_str(s).ok())
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 2048)
        .map(str::to_string)
}

/// An `https://` URL without credentials or fragment (`http://localhost` for draft apps).
pub fn safe_url(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    let local = u.scheme() == "http"
        && matches!(u.host_str(), Some("localhost") | Some("127.0.0.1"));
    (u.scheme() == "https" || local)
        && u.username().is_empty()
        && u.password().is_none()
        && u.fragment().is_none()
        && u.host_str().is_some()
}

impl MeetingSurface {
    fn from_json(v: &Value) -> Option<Self> {
        let url = str_at(v, "url").filter(|u| safe_url(u))?;
        let origins: Vec<String> = v
            .get("origins")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|o| safe_url(o))
                    .take(10)
                    .map(|o| o.trim_end_matches('/').to_string())
                    .collect()
            })
            .unwrap_or_default();
        Some(Self { url, origins })
    }
}

impl Grant {
    pub fn from_json(v: &Value) -> Option<Self> {
        let app = v.get("app").filter(|a| a.is_object());
        let app_field = |k: &str| app.and_then(|a| str_at(a, k)).or_else(|| str_at(v, k));
        let surface = app
            .and_then(|a| a.get("meetingSurface"))
            .or_else(|| v.get("meetingSurface"))
            .and_then(MeetingSurface::from_json);
        Some(Self {
            id: uuid_at(v, &["id", "grantId"])?,
            app_id: uuid_at(v, &["appId", "clientId", "applicationId"])
                .or_else(|| app.and_then(|a| uuid_at(a, &["clientId", "id", "appId"])))?,
            target: str_at(v, "target").unwrap_or_else(|| "USER".into()),
            organization_id: uuid_at(v, &["organizationId"]),
            account_id: uuid_at(v, &["accountId"]),
            scopes: v
                .get("scopes")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().or_else(|| s.get("name")?.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            status: str_at(v, "status").unwrap_or_else(|| "ACTIVE".into()),
            app_name: app_field("name").or_else(|| str_at(v, "appName")),
            developer_name: app_field("developerName"),
            logo_url: app_field("logoUrl").filter(|u| safe_url(u)),
            meeting_surface: surface,
            sub: str_at(v, "sub").filter(|s| s.len() <= 128),
            created_at: str_at(v, "createdAt"),
        })
    }

    pub fn is_active(&self) -> bool {
        self.status == "ACTIVE"
    }

    /// The listed grants of a `{items:[grant]}` answer (malformed entries dropped).
    pub fn list_of(body: &Value) -> Option<Vec<Self>> {
        Some(
            body.get("items")?
                .as_array()?
                .iter()
                .filter_map(Self::from_json)
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn grants_are_parsed_defensively() {
        let (id, app, acct) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let g = Grant::from_json(&json!({
            "id": id, "appId": app, "target": "USER", "accountId": acct,
            "scopes": ["meetings:read", {"name": "profile:read"}],
            "app": { "name": "Cal", "developerName": "Acme", "logoUrl": "javascript:alert(1)",
                     "meetingSurface": { "url": "https://app.example/panel", "origins": ["https://app.example/", "http://evil.example"] } },
            "sub": "abc",
        }))
        .unwrap();
        assert_eq!((g.id, g.app_id, g.account_id), (id, app, Some(acct)));
        assert_eq!(g.scopes, vec!["meetings:read", "profile:read"]);
        assert_eq!(g.app_name.as_deref(), Some("Cal"));
        assert_eq!(g.logo_url, None, "unsafe URL dropped");
        let s = g.meeting_surface.unwrap();
        assert_eq!(s.origins, vec!["https://app.example"]);
        assert!(g.is_active());
        assert!(Grant::from_json(&json!({ "id": id })).is_none());
        assert!(Grant::from_json(&json!({
            "id": id, "appId": app, "meetingSurface": { "url": "http://x.example/" }
        }))
        .unwrap()
        .meeting_surface
        .is_none());
    }

    #[test]
    fn urls_must_be_https_without_credentials_or_fragments() {
        assert!(safe_url("https://a.example/cb"));
        assert!(safe_url("http://localhost:3000/cb"));
        assert!(!safe_url("http://a.example/cb"));
        assert!(!safe_url("https://u:p@a.example/cb"));
        assert!(!safe_url("https://a.example/cb#x"));
        assert!(!safe_url("not a url"));
    }
}
