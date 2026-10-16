//! A public-network world for Part H control tests: scripted Privacy Relay and
//! Telephony, a permissive NG routing policy (app-to-phone, phone-to-app, callback relay),
//! a spend policy, a live-telecom PRODUCTION environment and the initiator's device. Each
//! helper exposes the side effects a control must prevent (Privacy Relay authorizations,
//! grants, spend reservations, Telephony commands and originates, Calls).

use std::sync::Arc;

use axum::http::Method;
use calling::{
    domain::EnvironmentKind,
    platform::{credentials::IssuedCredential, scopes::names},
    routing::{
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        RoutingClass,
    },
    telephony::spend::{SpendPolicy, SpendScope},
};
use cf_service_auth::{OutboundServiceRequest, ServiceId};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    app::{AppWorld, Device},
    privacy::FakePrivacyRelay,
    telephony::{FakePublicMedia, FakeTelephony},
    TestResponse,
};

/// Routing policy: NG and GB destinations, every public class (and enterprise-internal
/// calls) allowed.
pub fn public_policy() -> RoutingPolicy {
    RoutingPolicy {
        allowed_route_classes: vec![
            RoutingClass::AppRtc,
            RoutingClass::EnterpriseInternal,
            RoutingClass::AppToPstn,
            RoutingClass::PstnToApp,
            RoutingClass::CallbackRelay,
        ],
        public_calling_enabled: true,
        callback_relay_enabled: true,
        destinations: DestinationPolicy {
            allow: vec!["NG".into(), "GB".into()],
            deny: vec![],
            default: CountryDecision::Deny,
        },
        ..RoutingPolicy::default()
    }
}

pub fn spend_policy() -> SpendPolicy {
    SpendPolicy {
        currency: "NGN".into(),
        leg_allowance_minor: 5_000,
        max_leg_seconds: 3600,
        daily_limit_minor: 500_000,
        monthly_limit_minor: 5_000_000,
        max_concurrent_legs: 50,
        max_originations_per_minute: 100,
        max_per_party_per_hour: 100,
        max_per_principal_per_hour: None,
        max_per_service_account_per_hour: None,
        max_per_context_per_hour: None,
        enabled: true,
    }
}

pub struct PublicWorld {
    pub w: AppWorld,
    pub privacy: Arc<FakePrivacyRelay>,
    pub telephony: Arc<FakeTelephony>,
    pub media: Arc<FakePublicMedia>,
    /// A verified NG contact Privacy Relay knows.
    pub contact: Uuid,
    pub session: Uuid,
    /// The initiator's device.
    pub caller: Device,
}

impl PublicWorld {
    pub async fn new() -> Option<Self> {
        Self::configured(|_| {}).await
    }

    /// A world whose Calling runtime is adjusted by `configure`.
    pub async fn configured(
        configure: impl FnOnce(&mut calling::state::CallingRuntime),
    ) -> Option<Self> {
        let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
        let telephony = Arc::new(FakeTelephony::healthy());
        let media = Arc::new(FakePublicMedia::default());
        let w = AppWorld::with_public_network_configured(
            privacy.clone(),
            telephony.clone(),
            Some(media.clone()),
            configure,
        )
        .await?;
        privacy.allow(w.w.initiator.id().as_uuid());
        let contact = privacy.add_contact("NG");
        let caller = w.device(&w.w.initiator, "caller-main").await;
        let x = Self {
            w,
            privacy,
            telephony,
            media,
            contact,
            session: Uuid::new_v4(),
            caller,
        };
        x.set_policy(&public_policy()).await;
        x.set_spend(spend_policy()).await;
        Some(x)
    }

    pub fn tenant(&self) -> Uuid {
        self.w.w.scope().tenant_id().as_uuid()
    }

    pub fn environment(&self) -> Uuid {
        self.w.w.scope().environment_id().as_uuid()
    }

    pub async fn set_policy(&self, policy: &RoutingPolicy) {
        let scope = self.w.w.scope();
        calling::routing::store::store_policy(
            self.w.h().conn(),
            scope.tenant_id().as_uuid(),
            scope.application_id().as_uuid(),
            scope.environment_id().as_uuid(),
            policy,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    }

    pub async fn set_spend(&self, p: SpendPolicy) {
        let scope = self.w.w.scope();
        calling::telephony::spend::store_policy(
            self.w.h().conn(),
            SpendScope {
                tenant_id: scope.tenant_id().as_uuid(),
                application_id: scope.application_id().as_uuid(),
                environment_id: scope.environment_id().as_uuid(),
            },
            &p,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    }

    /// `POST /v1/calls` from the initiator to a protected contact (APP_TO_PSTN).
    pub async fn call_to(&self, contact: Uuid, key: &str) -> TestResponse {
        self.w
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "initiatorPrincipalId": self.w.w.initiator.id(),
                    "initiatorEndpointId": self.caller.endpoint,
                    "targetContactReference": contact,
                    "privacyRelaySessionId": self.session,
                }),
                &[("idempotency-key", key)],
            )
            .await
    }

    /// `POST /v1/calls` requesting a callback relay from `mine` to `theirs`.
    pub async fn callback_to(&self, mine: Uuid, theirs: Uuid, key: &str) -> TestResponse {
        self.w
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "callbackRequested": true,
                    "initiatorPrincipalId": self.w.w.initiator.id(),
                    "initiatorContactReference": mine,
                    "targetContactReference": theirs,
                    "privacyRelaySessionId": self.session,
                }),
                &[("idempotency-key", key)],
            )
            .await
    }

    /// One integer of SQL.
    pub async fn count(&self, sql: &str) -> i64 {
        super::scalar_i64(self.w.h().conn(), sql).await
    }

    /// Calls, Telephony commands and spend reservations of this environment.
    pub async fn effects(&self) -> (i64, i64, i64) {
        let env = self.environment();
        (
            self.count(&format!(
                "SELECT count(*) FROM calls WHERE environment_id = '{env}'"
            ))
            .await,
            self.count(&format!(
                "SELECT count(*) FROM call_telephony_commands c JOIN calls k ON k.id = c.call_id \
                 WHERE k.environment_id = '{env}'"
            ))
            .await,
            self.count(&format!(
                "SELECT count(*) FROM call_spend_reservations r JOIN calls k ON k.id = r.call_id \
                 WHERE k.environment_id = '{env}'"
            ))
            .await,
        )
    }

    /// A credential with `enterprise.config.write` (application-level block rules).
    pub async fn app_admin(&self) -> IssuedCredential {
        super::enterprise::allow_enterprise_scopes(self.w.h(), &self.w.w.consumer).await;
        super::enterprise::credential(
            self.w.h(),
            &self.w.w.consumer,
            EnvironmentKind::Production,
            &[
                names::ENTERPRISE_CONFIG_READ,
                names::ENTERPRISE_CONFIG_WRITE,
            ],
        )
        .await
    }

    /// `POST /v1/blocks` with `token`.
    pub async fn block(&self, token: &str, body: Value) -> TestResponse {
        self.w
            .h()
            .request(Method::POST, "/v1/blocks", Some(token), Some(body))
            .await
    }

    /// The environment's inbound owner is the world's credential.
    pub async fn configure_owner(&self) {
        let credential = self.w.w.token.credential.id.as_uuid();
        super::exec(
            self.w.h().conn(),
            &format!(
                "INSERT INTO call_inbound_owners (tenant_id, application_id, environment_id, \
                   service_account_id, credential_id, enabled, revision, created_at, updated_at) \
                 SELECT tenant_id, application_id, environment_id, service_account_id, id, true, 1, \
                   now(), now() FROM platform_api_credentials WHERE id = '{credential}' \
                 ON CONFLICT (environment_id) DO NOTHING"
            ),
        )
        .await
        .unwrap();
    }

    /// Telephony's inbound offer from `caller_contact` to the application `targets`.
    pub fn offer(&self, telephony_leg: Uuid, caller_contact: Uuid, targets: &[Uuid]) -> Value {
        let s = self.w.w.scope();
        json!({
            "schemaVersion": "1",
            "telephonyLegId": telephony_leg,
            "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
            "scope": {
                "tenantId": s.tenant_id().as_uuid(),
                "applicationId": s.application_id().as_uuid(),
                "environmentId": s.environment_id().as_uuid(),
            },
            "inboundResolution": {
                "decisionId": Uuid::new_v4(),
                "contextId": self.privacy.context_id,
                "contextVersion": 1,
                "relaySessionId": self.session,
                "relaySessionVersion": 1,
                "assignmentId": Uuid::new_v4(),
                "assignmentGeneration": 1,
                "validUntil": chrono::Utc::now() + chrono::Duration::hours(1),
            },
            "callerContactReference": caller_contact,
            "allowedTargets": targets.iter().map(|t| json!({ "principalId": t })).collect::<Vec<_>>(),
            "maxDurationSeconds": 3600,
        })
    }

    /// POST `body` to the internal `path` signed as Telephony.
    pub async fn as_telephony(&self, path: &str, body: Value) -> TestResponse {
        let signed = self
            .w
            .h()
            .peers
            .telephony
            .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
            .unwrap();
        let headers: Vec<(String, String)> = signed
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap().to_string()))
            .collect();
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        self.w
            .h()
            .request_with(Method::POST, path, None, Some(body), &refs)
            .await
    }
}

/// Another tenant on the same deployment (same scripted Privacy Relay and Telephony),
/// set up for public calls like the world's own.
pub struct OtherTenant {
    pub consumer: super::Consumer,
    pub token: IssuedCredential,
    pub admin: IssuedCredential,
    pub initiator: calling::platform::principals::ResolvedPrincipal,
    pub device: Uuid,
}

impl OtherTenant {
    /// `POST /v1/calls` from this tenant's initiator to `contact`.
    pub async fn call_to(&self, x: &PublicWorld, contact: Uuid, key: &str) -> TestResponse {
        x.w.h()
            .request_with(
                Method::POST,
                "/v1/calls",
                Some(self.token.secret.expose_once()),
                Some(json!({
                    "kind": "VOICE",
                    "initiatorPrincipalId": self.initiator.id(),
                    "initiatorEndpointId": self.device,
                    "targetContactReference": contact,
                    "privacyRelaySessionId": Uuid::new_v4(),
                })),
                &[("idempotency-key", key)],
            )
            .await
    }
}

impl PublicWorld {
    pub async fn other_tenant(&self, slug: &str) -> OtherTenant {
        let h = self.w.h();
        let c = h
            .consumer(slug, calling::domain::TenantClassification::External)
            .await;
        super::enterprise::allow_enterprise_scopes(h, &c).await;
        let token = super::enterprise::credential(
            h,
            &c,
            EnvironmentKind::Production,
            &[
                names::CALLS_READ,
                names::CALLS_WRITE,
                names::PRINCIPALS_READ,
            ],
        )
        .await;
        let admin = super::enterprise::credential(
            h,
            &c,
            EnvironmentKind::Production,
            &[
                names::ENTERPRISE_CONFIG_READ,
                names::ENTERPRISE_CONFIG_WRITE,
            ],
        )
        .await;
        let env = c.env(EnvironmentKind::Production).clone();
        h.platform
            .set_live_telecom_allowed(env.id, true)
            .await
            .unwrap();
        calling::routing::store::store_policy(
            h.conn(),
            c.tenant.id.as_uuid(),
            c.application.id.as_uuid(),
            env.id.as_uuid(),
            &public_policy(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        calling::telephony::spend::store_policy(
            h.conn(),
            SpendScope {
                tenant_id: c.tenant.id.as_uuid(),
                application_id: c.application.id.as_uuid(),
                environment_id: env.id.as_uuid(),
            },
            &spend_policy(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let ctx = h.ctx(token.secret.expose_once()).await;
        let initiator =
            super::calls::resolve(h, &c, EnvironmentKind::Production, &ctx, "other-initiator")
                .await;
        self.privacy.allow(initiator.id().as_uuid());
        let r = h
            .request(
                Method::POST,
                "/v1/calling/endpoints",
                Some(token.secret.expose_once()),
                Some(
                    json!({ "principalId": initiator.id(), "endpointKey": "other-main",
                             "endpointKind": "MOBILE",
                             "capabilities": { "audio": true, "video": true } }),
                ),
            )
            .await;
        assert_eq!(r.status, axum::http::StatusCode::CREATED, "{}", r.text);
        OtherTenant {
            consumer: c,
            token,
            admin,
            initiator,
            device: r.json["endpointId"].as_str().unwrap().parse().unwrap(),
        }
    }
}

/// Every standalone all-digit token of `text` long enough to be a national-format
/// telephone number (234… or 44…, at least ten digits). Tokens are split on everything but
/// ASCII letters, digits and '-', so a digit run inside a random identifier — a hex hash,
/// a UUID group, base64 — belongs to a larger token and is not mistaken for a number,
/// while a number stored or returned as a value is its own token whatever surrounds it
/// (quotes, commas, spaces, '+', JSON).
pub fn number_like_runs(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|t| {
            t.len() >= 10
                && (t.starts_with("234") || t.starts_with("44"))
                && t.bytes().all(|b| b.is_ascii_digit())
        })
        .map(str::to_string)
        .collect()
}
