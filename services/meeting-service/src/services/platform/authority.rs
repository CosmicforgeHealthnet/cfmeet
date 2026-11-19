//! Platform Access as the authority of gateway-forwarded machine callers.
//!
//! The Meeting Service never verifies a customer credential. For a request the API
//! Gateway forwarded (its signed delegated context naming the credential it
//! authenticated), Platform Access re-fetches that credential's LIVE authority chain:
//! `POST /internal/v1/credentials/delegated-authority` (permission
//! `platform-credential.authenticate`, the delegated context attached to the Meeting
//! Service's own outbound token, no request body). Revocation, expiry and suspension
//! therefore apply on the very next request. The chain must be exactly the delegated
//! tenant / application / environment / service account / credential (checked by
//! Platform Access and again here); anything else is a refusal. Platform Access
//! unreachable is [`AuthorityUnavailable`] (the request is refused with 503).

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalResponse, InternalServiceClient},
    CorrelationId, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use serde_json::Value;
use uuid::Uuid;

use crate::services::actor::PlatformActor;

/// Platform Access's re-verification of a gateway-forwarded credential.
pub const DELEGATED_AUTHORITY_PATH: &str = "/internal/v1/credentials/delegated-authority";

/// The kind of an OAuth credential (ADR 0064, Platform Access `credentialKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OauthCredentialKind {
    /// `cfoa_`: an app acting for one CF Meet user (principal-restricted to that user).
    Access,
    /// `cfob_`: an organization install's bot (never a meetings actor).
    Bot,
}

/// ADR 0064: what Platform Access reports about an OAuth credential (absent for API keys).
///
/// Read defensively from the authentication answer: `credentialKind`, `appId` and
/// `grantId` (on the authority chain or next to it), the principal restriction and, when
/// Platform Access includes it, the restricted principal's reference (`cfmeet-user:<uuid>`
/// for a USER grant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthCredential {
    pub kind: OauthCredentialKind,
    pub app_id: Uuid,
    pub grant_id: Uuid,
    /// The single principal the credential is restricted to (`principalRestriction`).
    pub restricted_principal: Option<Uuid>,
    /// The restricted principal's reference, when Platform Access reports it.
    pub principal_reference: Option<String>,
    /// When the presented credential was issued (`credential.createdAt`), for the account
    /// revocation epoch.
    pub issued_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl OauthCredential {
    /// `None`: not an OAuth credential. `Some(Err(()))`: an OAuth credential whose answer
    /// is incomplete (refused).
    pub fn from_answer(body: &Value, authority: &Value) -> Option<Result<Self, ()>> {
        let field = |name: &str| authority.get(name).or_else(|| body.get(name));
        let kind = match field("credentialKind").and_then(Value::as_str)? {
            "OAUTH_ACCESS" => OauthCredentialKind::Access,
            "OAUTH_BOT" => OauthCredentialKind::Bot,
            // API keys (and any future non-OAuth kind) keep the machine-caller rules.
            _ => return None,
        };
        let uuid = |name: &str| {
            field(name)
                .and_then(Value::as_str)
                .and_then(|v| Uuid::parse_str(v).ok())
        };
        let (Some(app_id), Some(grant_id)) = (uuid("appId"), uuid("grantId")) else {
            return Some(Err(()));
        };
        let restriction: Option<Vec<Uuid>> = field("principalRestriction")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                    .collect()
            });
        let restricted_principal = match restriction.as_deref() {
            Some([one]) => Some(*one),
            // An OAuth credential is restricted to EXACTLY one principal.
            _ => return Some(Err(())),
        };
        let principal_reference = ["restrictedPrincipal", "principal"]
            .iter()
            .filter_map(|k| field(k))
            .filter(|p| {
                p.get("id")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .is_none_or(|id| Some(id) == restricted_principal)
            })
            .find_map(|p| p.get("reference").and_then(Value::as_str))
            .or_else(|| field("principalReference").and_then(Value::as_str))
            .map(str::to_string);
        let issued_at = authority
            .get("credential")
            .and_then(|c| c.get("createdAt").or_else(|| c.get("notBefore")))
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        Some(Ok(Self {
            kind,
            app_id,
            grant_id,
            restricted_principal,
            principal_reference,
            issued_at,
        }))
    }

    /// The CF Meet account of a `cfmeet-user:<uuid>` reference (canonical form only).
    pub fn account_of_reference(reference: &str) -> Option<Uuid> {
        let id = Uuid::parse_str(reference.strip_prefix("cfmeet-user:")?).ok()?;
        (format!("cfmeet-user:{id}") == reference).then_some(id)
    }
}

/// A parsed authority chain (contract `cfmeet.platform-access.identity` AuthorityChain).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityChain {
    pub actor: PlatformActor,
    /// `authorityMode == FULL`; SAFETY_ONLY chains are refused (no meetings operation is
    /// a safety operation).
    pub full_authority: bool,
    /// ADR 0064: present when the credential is an OAuth token (`credentialKind`);
    /// `Some(Err(()))` when its OAuth fields are incomplete (always refused).
    pub oauth: Option<Result<OauthCredential, ()>>,
}

impl AuthorityChain {
    pub fn from_authority(authority: &Value) -> Option<Self> {
        let id = |path: &[&str]| -> Option<Uuid> {
            let mut v = authority;
            for p in path {
                v = v.get(*p)?;
            }
            Uuid::parse_str(v.as_str()?).ok()
        };
        let kind = authority.get("environment")?.get("kind")?.as_str()?;
        if !matches!(kind, "SANDBOX" | "TEST" | "PRODUCTION") {
            return None;
        }
        let scopes: BTreeSet<String> = authority
            .get("scopes")?
            .as_array()?
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        Some(Self {
            actor: PlatformActor {
                tenant_id: id(&["tenant", "id"])?,
                application_id: id(&["application", "id"])?,
                environment_id: id(&["environment", "id"])?,
                environment_kind: kind.to_string(),
                service_account_id: id(&["serviceAccount", "id"])?,
                credential_id: id(&["credential", "id"])?,
                scopes,
            },
            full_authority: authority.get("authorityMode")?.as_str()? == "FULL",
            oauth: OauthCredential::from_answer(&Value::Null, authority),
        })
    }

    /// [`Self::from_authority`] reading the OAuth fields from the whole answer too.
    pub fn from_answer(body: &Value) -> Option<Self> {
        let authority = body.get("authority")?;
        let mut chain = Self::from_authority(authority)?;
        chain.oauth = OauthCredential::from_answer(body, authority);
        Some(chain)
    }

    /// Whether this chain is exactly the credential and scope `delegated` names (defence
    /// in depth over Platform Access's own check).
    pub fn is_named_by(&self, delegated: &DelegatedPlatformContext) -> bool {
        let a = &self.actor;
        delegated.initiating_credential_id() == Some(a.credential_id)
            && delegated.tenant_id() == a.tenant_id
            && delegated.application_id() == a.application_id
            && delegated.application_environment_id() == a.environment_id
            && delegated.initiating_service_account_id() == a.service_account_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    Authenticated(Box<AuthorityChain>),
    /// Refused by the authority (bounded reason, for operator logs only).
    Rejected {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("the identity authority is unavailable: {0}")]
pub struct AuthorityUnavailable(pub String);

#[allow(clippy::double_must_use)]
#[async_trait]
pub trait CredentialAuthority: Send + Sync {
    /// The CURRENT authority chain of the credential the API Gateway authenticated, named
    /// by `delegated.initiating_credential_id()`. `correlation_id` continues the forwarded
    /// operation.
    async fn delegated_authority(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable>;
}

pub type SharedCredentialAuthority = Arc<dyn CredentialAuthority>;

/// Platform Access over authenticated internal HTTP.
pub struct PlatformAccessAuthority {
    client: Arc<InternalServiceClient>,
}

impl PlatformAccessAuthority {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

/// Maps Platform Access's `AuthenticateResponse`.
fn authentication_of(response: InternalResponse) -> Result<Authentication, AuthorityUnavailable> {
    if response.status() != 200 {
        return Err(AuthorityUnavailable(format!(
            "HTTP {} {}",
            response.status(),
            response.error_code().unwrap_or_default()
        )));
    }
    let body: Value = response
        .json()
        .map_err(|_| AuthorityUnavailable("malformed authentication response".into()))?;
    match body.get("outcome").and_then(Value::as_str) {
        Some("AUTHENTICATED") => AuthorityChain::from_answer(&body)
            .map(|c| Authentication::Authenticated(Box::new(c)))
            .ok_or_else(|| AuthorityUnavailable("malformed authority chain".into())),
        Some("REJECTED") => Ok(Authentication::Rejected {
            reason: body
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("rejected")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(64)
                .collect(),
        }),
        _ => Err(AuthorityUnavailable(
            "unknown authentication outcome".into(),
        )),
    }
}

#[async_trait]
impl CredentialAuthority for PlatformAccessAuthority {
    async fn delegated_authority(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable> {
        // No request body: Platform Access reads the credential id ONLY from the delegated
        // context signed into this hop's service token.
        let request =
            OutboundServiceRequest::continuing(ServiceId::platform_access(), correlation_id)
                .with_delegated_context(delegated.clone());
        let response = self
            .client
            .exchange_json::<()>(&request, Method::POST, DELEGATED_AUTHORITY_PATH, None)
            .await
            .map_err(|e: InternalCallError| AuthorityUnavailable(e.to_string()))?;
        authentication_of(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chain(kind: &str, mode: &str) -> Value {
        let id = || Uuid::new_v4().to_string();
        json!({
            "authorityMode": mode,
            "tenant": { "id": id(), "classification": "EXTERNAL" },
            "application": { "id": id() },
            "environment": { "id": id(), "kind": kind },
            "serviceAccount": { "id": id() },
            "credential": { "id": id() },
            "scopes": ["meetings.read", "calls.read"],
        })
    }

    #[test]
    fn the_actor_comes_only_from_the_authority_chain() {
        let c = AuthorityChain::from_authority(&chain("SANDBOX", "FULL")).unwrap();
        assert!(c.full_authority);
        assert_eq!(c.actor.environment_kind, "SANDBOX");
        assert!(c.actor.has_scope("meetings.read") && !c.actor.has_scope("meetings.write"));
        assert!(
            !AuthorityChain::from_authority(&chain("TEST", "SAFETY_ONLY"))
                .unwrap()
                .full_authority
        );
        assert!(AuthorityChain::from_authority(&chain("STAGING", "FULL")).is_none());
        assert!(AuthorityChain::from_authority(&json!({})).is_none());
    }

    #[test]
    fn oauth_credentials_are_recognized_and_must_be_complete() {
        let plain = AuthorityChain::from_authority(&chain("PRODUCTION", "FULL")).unwrap();
        assert_eq!(plain.oauth, None, "API keys are not OAuth credentials");
        let principal = Uuid::new_v4();
        let account = Uuid::new_v4();
        let (app, grant) = (Uuid::new_v4(), Uuid::new_v4());
        let mut a = chain("PRODUCTION", "FULL");
        a["credential"]["createdAt"] = json!("2026-11-18T10:00:00Z");
        a["principalRestriction"] = json!([principal]);
        let body = json!({
            "outcome": "AUTHENTICATED",
            "authority": a,
            "credentialKind": "OAUTH_ACCESS",
            "appId": app,
            "grantId": grant,
            "restrictedPrincipal": { "id": principal, "reference": format!("cfmeet-user:{account}") },
        });
        let c = AuthorityChain::from_answer(&body).unwrap();
        let o = c.oauth.unwrap().unwrap();
        assert_eq!(o.kind, OauthCredentialKind::Access);
        assert_eq!((o.app_id, o.grant_id), (app, grant));
        assert_eq!(o.restricted_principal, Some(principal));
        assert_eq!(
            o.principal_reference
                .as_deref()
                .and_then(OauthCredential::account_of_reference),
            Some(account)
        );
        assert!(o.issued_at.is_some());
        // Kind on the chain itself, bot tokens.
        let mut a2 = a.clone();
        a2["credentialKind"] = json!("OAUTH_BOT");
        a2["appId"] = json!(app);
        a2["grantId"] = json!(grant);
        let o = AuthorityChain::from_authority(&a2)
            .unwrap()
            .oauth
            .unwrap()
            .unwrap();
        assert_eq!(o.kind, OauthCredentialKind::Bot);
        // Incomplete: no grant, or not exactly one restricted principal.
        let mut bad = body.clone();
        bad.as_object_mut().unwrap().remove("grantId");
        assert_eq!(AuthorityChain::from_answer(&bad).unwrap().oauth, Some(Err(())));
        let mut bad = body.clone();
        bad["authority"]["principalRestriction"] = json!(null);
        assert_eq!(AuthorityChain::from_answer(&bad).unwrap().oauth, Some(Err(())));
        // References are canonical only.
        assert_eq!(OauthCredential::account_of_reference(&format!("cfmeet-user:{}", account.simple())), None);
        assert_eq!(OauthCredential::account_of_reference(&format!("app:{account}")), None);
    }

    #[test]
    fn a_forwarded_chain_must_be_exactly_the_delegated_credential_and_scope() {
        use cf_service_auth::ApplicationEnvironmentKind as K;
        let c = AuthorityChain::from_authority(&chain("SANDBOX", "FULL")).unwrap();
        let a = &c.actor;
        let scope = DelegatedPlatformContext::new(
            a.tenant_id,
            a.application_id,
            a.environment_id,
            K::Sandbox,
            a.service_account_id,
        );
        assert!(!c.is_named_by(&scope), "no credential named");
        assert!(c.is_named_by(&scope.clone().with_initiating_credential(a.credential_id)));
        assert!(!c.is_named_by(&scope.with_initiating_credential(Uuid::new_v4())));
        let foreign = DelegatedPlatformContext::new(
            Uuid::new_v4(),
            a.application_id,
            a.environment_id,
            K::Sandbox,
            a.service_account_id,
        )
        .with_initiating_credential(a.credential_id);
        assert!(!c.is_named_by(&foreign));
    }
}
