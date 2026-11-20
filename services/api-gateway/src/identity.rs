//! Customer authentication — Platform Access is the ONLY credential authority.
//!
//! The gateway never verifies a credential itself: it asks Platform Access
//! (`POST /internal/v1/credentials/authenticate`, permission
//! `platform-credential.authenticate`) and takes tenant, application, environment, service
//! account and scopes ONLY from the authority chain Platform Access returns. A tenant,
//! project or environment id in a request (header, query or body) never becomes authority.
//! Platform Access unreachable means the request is refused (fail closed).
//!
//! CF Meet Apps OAuth credentials (ADR 0064; `cfoa_` access and `cfob_` bot tokens) are
//! authenticated by the same call. Platform Access additionally returns the
//! `credentialKind` (`OAUTH_ACCESS` | `OAUTH_BOT`), the `appId` and the `grantId` (read
//! from the authority chain, its `credential` snapshot, or the response itself, in that
//! order) and a `principalRestriction` to exactly the user (or bot) principal; the
//! authority chain is the install target (its FIRST_PARTY triple and installation
//! service account).

use std::{collections::BTreeSet, sync::Arc};

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext, DelegatedPrincipalKind,
    OutboundServiceRequest, ServiceId,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// Trust classification of the tenant (Platform Access `TenantClassification`).
/// First-party CosmicForge products and external customers use the same routes; they
/// differ only in authorized scopes and this classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustClassification {
    FirstParty,
    External,
}

/// The kind of an OAuth credential (Platform Access `credentialKind`, ADR 0064).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// `cfoa_…`: acts for the granting CF Meet user (`NATIVE_CF_USER`).
    OauthAccess,
    /// `cfob_…`: the organization install's bot (`BOT_OR_SERVICE`).
    OauthBot,
}

impl CredentialKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CredentialKind::OauthAccess => "OAUTH_ACCESS",
            CredentialKind::OauthBot => "OAUTH_BOT",
        }
    }

    /// The delegated principal kind of the token's restricted principal.
    pub fn principal_kind(&self) -> DelegatedPrincipalKind {
        match self {
            CredentialKind::OauthAccess => DelegatedPrincipalKind::NativeCfUser,
            CredentialKind::OauthBot => DelegatedPrincipalKind::BotOrService,
        }
    }
}

/// What Platform Access says about an OAuth credential (identifiers only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OauthCredential {
    pub kind: CredentialKind,
    pub app_id: Uuid,
    pub grant_id: Uuid,
}

/// The authenticated customer of a request, from Platform Access's authority chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedCustomer {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: ApplicationEnvironmentKind,
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    pub classification: TrustClassification,
    /// FULL authority; SAFETY_ONLY contexts are refused at the ingress.
    pub full_authority: bool,
    pub scopes: BTreeSet<String>,
    /// The authorized-principal restriction (`None`: unrestricted). A malformed list is
    /// kept as an empty one (it then never names exactly one principal).
    pub principal_restriction: Option<Vec<Uuid>>,
    /// `Some` for CF Meet Apps OAuth credentials (ADR 0064); `None` for API keys.
    pub oauth: Option<OauthCredential>,
}

impl AuthenticatedCustomer {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }

    /// The single principal an OAuth credential is restricted to (`None`: not an OAuth
    /// credential, or a restriction that is not exactly one principal — refused).
    pub fn oauth_principal(&self) -> Option<Uuid> {
        self.oauth?;
        match self.principal_restriction.as_deref() {
            Some([principal]) => Some(*principal),
            _ => None,
        }
    }

    /// The signed delegated context forwarded upstream (identifiers only). For an OAuth
    /// credential the initiating principal is its single restricted principal (the wire
    /// format is unchanged; the receiver learns the acting app from the credential).
    pub fn delegated(&self) -> DelegatedPlatformContext {
        let context = DelegatedPlatformContext::new(
            self.tenant_id,
            self.application_id,
            self.environment_id,
            self.environment_kind,
            self.service_account_id,
        )
        // The receiver re-verifies this credential with Platform Access (revocation,
        // expiry and effective scopes are evaluated live, never trusted from here).
        .with_initiating_credential(self.credential_id);
        match (self.oauth, self.oauth_principal()) {
            (Some(oauth), Some(principal)) => {
                context.with_initiating_principal(principal, oauth.kind.principal_kind())
            }
            _ => context,
        }
    }

    /// Parses Platform Access's `AuthorityChain` (contract `cfmeet.platform-access.identity`).
    pub fn from_authority(authority: &Value) -> Option<Self> {
        Self::from_response(authority, &Value::Null)
    }

    /// [`from_authority`](Self::from_authority) with the OAuth fields (`credentialKind`,
    /// `appId`, `grantId`) also looked up in the whole authentication `response`.
    pub fn from_response(authority: &Value, response: &Value) -> Option<Self> {
        let id = |path: &[&str]| -> Option<Uuid> {
            let mut v = authority;
            for p in path {
                v = v.get(*p)?;
            }
            Uuid::parse_str(v.as_str()?).ok()
        };
        let environment_kind = match authority.get("environment")?.get("kind")?.as_str()? {
            "SANDBOX" => ApplicationEnvironmentKind::Sandbox,
            "TEST" => ApplicationEnvironmentKind::Test,
            "PRODUCTION" => ApplicationEnvironmentKind::Production,
            _ => return None,
        };
        let classification = match authority.get("tenant")?.get("classification")?.as_str()? {
            "FIRST_PARTY" => TrustClassification::FirstParty,
            "EXTERNAL" => TrustClassification::External,
            _ => return None,
        };
        let scopes = authority
            .get("scopes")?
            .as_array()?
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        let principal_restriction = match authority.get("principalRestriction") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => Some(
                items
                    .iter()
                    .map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                    .collect::<Option<Vec<_>>>()
                    .unwrap_or_default(),
            ),
            Some(_) => Some(Vec::new()),
        };
        // OAuth fields: the chain, its credential snapshot, then the response itself.
        let field = |name: &str| -> Option<&Value> {
            [authority, &authority["credential"], response]
                .into_iter()
                .find_map(|v| v.get(name).filter(|f| !f.is_null()))
        };
        let oauth = match field("credentialKind").map(|k| k.as_str()) {
            None => None,
            Some(Some(kind)) if !kind.starts_with("OAUTH") => None,
            Some(kind) => {
                let kind = match kind? {
                    "OAUTH_ACCESS" => CredentialKind::OauthAccess,
                    "OAUTH_BOT" => CredentialKind::OauthBot,
                    // An unknown OAuth kind never authenticates (malformed chain).
                    _ => return None,
                };
                let uuid = |name: &str| Uuid::parse_str(field(name)?.as_str()?).ok();
                Some(OauthCredential {
                    kind,
                    app_id: uuid("appId")?,
                    grant_id: uuid("grantId")?,
                })
            }
        };
        Some(Self {
            tenant_id: id(&["tenant", "id"])?,
            application_id: id(&["application", "id"])?,
            environment_id: id(&["environment", "id"])?,
            environment_kind,
            service_account_id: id(&["serviceAccount", "id"])?,
            credential_id: id(&["credential", "id"])?,
            classification,
            full_authority: authority.get("authorityMode")?.as_str()? == "FULL",
            scopes,
            principal_restriction,
            oauth,
        })
    }
}

/// Failed-authentication throttling reported by Platform Access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Throttle {
    pub retry_after_seconds: u64,
    pub threshold_crossed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    Authenticated(Box<AuthenticatedCustomer>),
    /// The credential does not authenticate (reason for logs only).
    Rejected {
        reason: String,
        throttle: Option<Throttle>,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("the identity authority is unavailable: {0}")]
pub struct AuthorityUnavailable(pub String);

#[async_trait::async_trait]
pub trait CustomerAuthority: Send + Sync {
    /// Authenticates `presented` as part of the operation `correlation`.
    async fn authenticate(
        &self,
        presented: &str,
        correlation: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable>;

    /// Whether the authority answers at all (readiness: every customer request needs it).
    async fn reachable(&self) -> bool {
        true
    }
}

/// Platform Access over authenticated internal HTTP.
pub struct PlatformAccessAuthority {
    client: Arc<InternalServiceClient>,
    /// `GET {base}/health` is the readiness probe (`None`: not probed, assumed up).
    health: Option<(url::Url, reqwest::Client)>,
}

impl PlatformAccessAuthority {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self {
            client,
            health: None,
        }
    }

    /// Probes `base`'s public `/health` for readiness (bounded: 1 s).
    pub fn with_health_probe(mut self, base: Option<&url::Url>) -> Self {
        self.health = base.and_then(|b| {
            let url = b.join("/health").ok()?;
            let http = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .ok()?;
            Some((url, http))
        });
        self
    }
}

#[async_trait::async_trait]
impl CustomerAuthority for PlatformAccessAuthority {
    async fn reachable(&self) -> bool {
        match &self.health {
            None => true,
            Some((url, http)) => matches!(
                http.get(url.clone()).send().await,
                Ok(r) if r.status().is_success()
            ),
        }
    }

    async fn authenticate(
        &self,
        presented: &str,
        correlation: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable> {
        let request = OutboundServiceRequest::continuing(ServiceId::platform_access(), correlation);
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/credentials/authenticate",
                &json!({ "credential": presented }),
            )
            .await
            .map_err(|e: InternalCallError| AuthorityUnavailable(e.to_string()))?;
        let body: Value = response
            .json()
            .map_err(|_| AuthorityUnavailable("malformed authentication response".into()))?;
        match body.get("outcome").and_then(Value::as_str) {
            Some("AUTHENTICATED") => body
                .get("authority")
                .and_then(|a| AuthenticatedCustomer::from_response(a, &body))
                .map(|c| Authentication::Authenticated(Box::new(c)))
                .ok_or_else(|| AuthorityUnavailable("malformed authority chain".into())),
            Some("REJECTED") => Ok(Authentication::Rejected {
                reason: body
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("rejected")
                    .chars()
                    .take(64)
                    .collect(),
                throttle: body
                    .get("throttle")
                    .and_then(|t| serde_json::from_value(t.clone()).ok()),
            }),
            _ => Err(AuthorityUnavailable(
                "unknown authentication outcome".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(kind: &str, classification: &str) -> Value {
        let id = || Uuid::new_v4().to_string();
        json!({
            "authorityMode": "FULL",
            "tenant": { "id": id(), "classification": classification },
            "application": { "id": id() },
            "environment": { "id": id(), "kind": kind },
            "serviceAccount": { "id": id() },
            "credential": { "id": id() },
            "scopes": ["sms.read"],
        })
    }

    #[test]
    fn the_customer_comes_only_from_a_well_formed_authority_chain() {
        let c = AuthenticatedCustomer::from_authority(&chain("SANDBOX", "EXTERNAL")).unwrap();
        assert_eq!(c.classification, TrustClassification::External);
        assert!(c.has_scope("sms.read") && !c.has_scope("sms.send"));
        let d = c.delegated();
        assert_eq!(d.tenant_id(), c.tenant_id);
        assert_eq!(d.application_environment_id(), c.environment_id);
        assert!(AuthenticatedCustomer::from_authority(&chain("STAGING", "EXTERNAL")).is_none());
        assert!(AuthenticatedCustomer::from_authority(&chain("SANDBOX", "PARTNER")).is_none());
        assert!(AuthenticatedCustomer::from_authority(&json!({})).is_none());
        assert_eq!((c.oauth, c.principal_restriction.clone()), (None, None));
        assert_eq!(d.initiating_principal(), None);
    }

    #[test]
    fn oauth_credentials_carry_their_single_restricted_principal() {
        let (app, grant, principal) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut a = chain("PRODUCTION", "FIRST_PARTY");
        a["credentialKind"] = json!("OAUTH_ACCESS");
        a["appId"] = json!(app);
        a["grantId"] = json!(grant);
        a["principalRestriction"] = json!([principal]);
        let c = AuthenticatedCustomer::from_authority(&a).unwrap();
        assert_eq!(
            c.oauth,
            Some(OauthCredential {
                kind: CredentialKind::OauthAccess,
                app_id: app,
                grant_id: grant
            })
        );
        assert_eq!(c.oauth_principal(), Some(principal));
        let d = c.delegated();
        assert_eq!(d.initiating_credential_id(), Some(c.credential_id));
        let p = d.initiating_principal().unwrap();
        assert_eq!(
            (p.principal_id, p.principal_kind),
            (principal, DelegatedPrincipalKind::NativeCfUser)
        );
        // The fields may also come with the credential snapshot or the response itself.
        let mut b = chain("PRODUCTION", "FIRST_PARTY");
        b["credential"]["credentialKind"] = json!("OAUTH_BOT");
        b["principalRestriction"] = json!([principal]);
        let response = json!({ "appId": app, "grantId": grant });
        let c = AuthenticatedCustomer::from_response(&b, &response).unwrap();
        assert_eq!(c.oauth.unwrap().kind, CredentialKind::OauthBot);
        assert_eq!(
            c.delegated().initiating_principal().unwrap().principal_kind,
            DelegatedPrincipalKind::BotOrService
        );
        // Not exactly one principal: no OAuth principal (the ingress refuses).
        for restriction in [
            json!(null),
            json!([]),
            json!([principal, Uuid::new_v4()]),
            json!(["not-a-uuid"]),
            json!("x"),
        ] {
            let mut x = a.clone();
            x["principalRestriction"] = restriction.clone();
            let c = AuthenticatedCustomer::from_authority(&x).unwrap();
            assert_eq!(c.oauth_principal(), None, "{restriction}");
            assert_eq!(c.delegated().initiating_principal(), None);
        }
        // An OAuth kind without its app / grant, or an unknown OAuth kind: malformed.
        for (k, v) in [
            ("appId", json!(null)),
            ("grantId", json!("x")),
            ("credentialKind", json!("OAUTH_REFRESH")),
        ] {
            let mut x = a.clone();
            x[k] = v;
            assert!(AuthenticatedCustomer::from_authority(&x).is_none(), "{k}");
        }
        // A non-OAuth kind is an ordinary credential.
        let mut x = chain("SANDBOX", "EXTERNAL");
        x["credentialKind"] = json!("API_KEY");
        assert_eq!(
            AuthenticatedCustomer::from_authority(&x).unwrap().oauth,
            None
        );
    }
}
