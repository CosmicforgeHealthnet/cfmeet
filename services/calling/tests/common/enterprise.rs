//! Enterprise calling fixtures (Part F): an application-calling world whose application
//! is allowed the enterprise scopes, with separate credentials for administration,
//! read-only administration and calling — so scope isolation is exercised, never assumed.

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, TenantClassification},
    platform::{credentials::IssuedCredential, limits::LimitValues, scopes::names},
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{app::AppWorld, Consumer, Harness, TestResponse};

pub const ENTERPRISE_SCOPES: &[&str] = &[
    names::ENTERPRISE_CONFIG_READ,
    names::ENTERPRISE_CONFIG_WRITE,
    names::ENTERPRISE_CALLS_USE,
    names::ENTERPRISE_VOICEMAIL_READ,
    names::ENTERPRISE_VOICEMAIL_MANAGE,
    names::ENTERPRISE_RECORDING_MANAGE,
    names::ENTERPRISE_RECORDING_READ,
];

/// An enterprise world: the AppWorld's credential (calls.*), plus an administrator
/// (`enterprise.config.*`), a read-only administrator and an enterprise caller
/// (`calls.*` + `enterprise.calls.use`).
pub struct EnterpriseWorld {
    pub a: AppWorld,
    pub admin: IssuedCredential,
    pub reader: IssuedCredential,
    pub caller: IssuedCredential,
}

/// Grants the enterprise scopes to `consumer`'s application.
pub async fn allow_enterprise_scopes(h: &Harness, consumer: &Consumer) {
    for scope in ENTERPRISE_SCOPES {
        h.platform
            .grant_application_scope(consumer.application.id, scope)
            .await
            .unwrap();
    }
}

/// Issues a credential with `scopes` and generous limits.
pub async fn credential(
    h: &Harness,
    consumer: &Consumer,
    kind: EnvironmentKind,
    scopes: &[&str],
) -> IssuedCredential {
    let token = h.issue(consumer.sa(kind), scopes).await;
    h.platform
        .set_credential_limits(
            token.credential.id,
            LimitValues {
                max_concurrent_communications: Some(100_000),
                requests_per_minute: Some(100_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    token
}

impl EnterpriseWorld {
    pub async fn new() -> Option<Self> {
        let a = AppWorld::new().await?;
        Some(Self::around(a).await)
    }

    /// A world whose deployment runs with explicit enterprise kill switches.
    pub async fn with_flags(flags: calling::enterprise::EnterpriseFlags) -> Option<Self> {
        use std::sync::Arc;
        let db = Arc::new(super::TestDb::new().await?);
        let media = Arc::new(super::fake_media::FakeMediaProvider::default());
        let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
        let mut runtime = super::test_runtime(Some(media.clone()), bus.clone());
        runtime.enterprise_flags = flags;
        let h = Harness::with_runtime(
            db,
            super::TEST_PEPPER,
            calling::domain::ManualClock::new(super::t0()),
            media,
            bus,
            runtime,
        );
        let w = super::calls::CallWorld::in_harness(h, "acme", EnvironmentKind::Production).await;
        Some(Self::around(AppWorld { w }).await)
    }

    pub async fn around(a: AppWorld) -> Self {
        let kind = a.w.kind;
        allow_enterprise_scopes(&a.w.h, &a.w.consumer).await;
        let admin = credential(
            &a.w.h,
            &a.w.consumer,
            kind,
            &[
                names::ENTERPRISE_CONFIG_READ,
                names::ENTERPRISE_CONFIG_WRITE,
            ],
        )
        .await;
        let reader = credential(
            &a.w.h,
            &a.w.consumer,
            kind,
            &[names::ENTERPRISE_CONFIG_READ],
        )
        .await;
        let caller = credential(
            &a.w.h,
            &a.w.consumer,
            kind,
            &[
                names::CALLS_READ,
                names::CALLS_WRITE,
                names::ENTERPRISE_CALLS_USE,
            ],
        )
        .await;
        Self {
            a,
            admin,
            reader,
            caller,
        }
    }

    pub fn h(&self) -> &Harness {
        self.a.h()
    }

    pub fn admin_token(&self) -> &str {
        self.admin.secret.expose_once()
    }

    pub async fn send(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> TestResponse {
        self.h().request(method, path, Some(token), body).await
    }

    pub async fn admin(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.send(self.admin_token(), method, path, body).await
    }

    /// POST as administrator; asserts 201 and returns the body.
    pub async fn create(&self, path: &str, body: Value) -> Value {
        let r = self.admin(Method::POST, path, Some(body)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{path}: {}", r.text);
        r.json
    }

    /// Another tenant in the same deployment with its own administrator.
    pub async fn other_tenant(&self, slug: &str) -> (Consumer, IssuedCredential) {
        let consumer = self
            .h()
            .consumer(slug, TenantClassification::External)
            .await;
        allow_enterprise_scopes(self.h(), &consumer).await;
        let admin = credential(
            self.h(),
            &consumer,
            self.a.w.kind,
            &[
                names::ENTERPRISE_CONFIG_READ,
                names::ENTERPRISE_CONFIG_WRITE,
            ],
        )
        .await;
        (consumer, admin)
    }

    /// A department with `name`.
    pub async fn department(&self, name: &str) -> Value {
        self.create("/v1/enterprise/departments", json!({ "name": name }))
            .await
    }
}

pub fn id_of(value: &Value, key: &str) -> Uuid {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} in {value}"))
        .parse()
        .unwrap()
}

/// A valid policy rule set (the built-in default's values).
pub fn default_rules() -> Value {
    json!({
        "internalCallingEnabled": true,
        "externalPublicCallingAllowed": false,
        "publicDestinationCountries": [],
        "forwardingAllowed": true,
        "publicForwardingAllowed": false,
        "spendPolicy": "APPLICATION_TELECOM_LIMITS",
        "voicemailAllowed": true,
        "transferAllowed": true,
        "maxRingGroupFanout": 10,
        "maxQueueCapacity": 50,
        "maxQueueWaitSeconds": 600,
        "maxRoutingHops": 8,
        "extensionFormat": "NUMERIC",
        "extensionMinLength": 3,
        "extensionMaxLength": 6,
        "adminCapabilities": ["MANAGE_ROUTING", "MANAGE_VOICEMAIL"]
    })
}

/// Asserts `value` conforms to `calling/enterprise/v1.schema.json#/$defs/<def>`.
pub fn conforms(def: &str, value: &Value) {
    super::contracts::check(
        &format!("calling/enterprise/v1.schema.json#/$defs/{def}"),
        value,
    );
}

impl EnterpriseWorld {
    pub fn caller_token(&self) -> &str {
        self.caller.secret.expose_once()
    }

    /// Registers an ENTERPRISE_MEMBER principal of this world's environment.
    pub async fn member(
        &self,
        reference: &str,
    ) -> calling::platform::principals::ResolvedPrincipal {
        let (view, _) = self
            .h()
            .platform
            .register_principal(
                self.a.w.consumer.env(self.a.w.kind).id,
                calling::platform::principals::NewPrincipal::External {
                    kind: calling::domain::PrincipalKind::EnterpriseMember,
                    external_reference: reference.to_string(),
                },
            )
            .await
            .unwrap();
        self.h()
            .state
            .principals
            .resolve(
                &self.a.w.ctx,
                &calling::platform::principals::PrincipalReference::Id(view.id),
            )
            .await
            .unwrap()
    }

    /// An extension `number` routing to `destination` (type, id).
    pub async fn extension(&self, number: &str, kind: &str, id: impl std::fmt::Display) -> Value {
        self.create(
            "/v1/enterprise/extensions",
            json!({
                "extensionNumber": number,
                "displayLabel": format!("Extension {number}"),
                "destination": { "type": kind, "id": id.to_string() },
            }),
        )
        .await
    }

    /// `POST /v1/calls` to `extension` as the enterprise caller.
    pub async fn call_extension(
        &self,
        kind: &str,
        initiator: &calling::platform::principals::ResolvedPrincipal,
        from: &super::app::Device,
        extension: &str,
        key: &str,
    ) -> TestResponse {
        self.h()
            .request_with(
                Method::POST,
                "/v1/calls",
                Some(self.caller_token()),
                Some(json!({
                    "kind": kind,
                    "initiatorPrincipalId": initiator.id(),
                    "initiatorEndpointId": from.endpoint,
                    "targetExtension": extension,
                })),
                &[("idempotency-key", key)],
            )
            .await
    }

    /// One integer of SQL against the Calling database.
    pub async fn count(&self, sql: &str) -> i64 {
        super::scalar_i64(self.h().conn(), sql).await
    }
}

/// Text rows of a query.
pub async fn strings(conn: &sea_orm::DatabaseConnection, sql: &str) -> Vec<String> {
    use sea_orm::ConnectionTrait;
    conn.query_all(sea_orm::Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
    .collect()
}
