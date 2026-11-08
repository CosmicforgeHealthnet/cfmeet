//! Final closure — the Organizations event ingress (`POST /internal/v1/organization-events`).
//!
//! Only the Organizations Service (`meeting-organization-events.apply`) delivers; each event
//! is applied once (dedupe on `eventId`, a different body under a known id is refused) and
//! monotonically per aggregate (an older version is STALE and changes nothing). Membership
//! events set the member's desired organization access for the offboarding worker.

mod common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use backend::services::organization_events;
use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use common::*;
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

fn signer(id: ServiceId, kid: &str) -> (ServiceTokenSigner, ServiceSigningKey) {
    let key = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    let public =
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse(kid).unwrap(), &key.to_pkcs8_pem()).unwrap();
    (
        ServiceTokenSigner::new(
            id,
            RuntimeEnvironment::Test,
            key,
            TokenTtl::default(),
            Arc::new(SystemClock),
        ),
        public,
    )
}

/// Organizations may deliver events; the Developer Platform is trusted for something else.
fn verifier(orgs: &ServiceSigningKey, dp: &ServiceSigningKey) -> Arc<ServiceTokenVerifier> {
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str]| {
        json!({
        "issuer": id, "audience": "cfmeet.meeting-service", "runtime_environment": "test",
        "keys": [k.public_jwk()], "permissions": perms })
    };
    let bundle = json!({ "bundle_version": 1, "receiver": "cfmeet.meeting-service", "runtime_environment": "test",
    "trusted_issuers": [
        issuer("cfmeet.organizations", orgs, &["meeting-organization-events.apply"]),
        issuer("cfmeet.developer-platform", dp, &["user-session.verify"]),
    ]});
    let policy = TrustPolicy::from_json(
        &bundle.to_string(),
        &ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
    )
    .unwrap();
    Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)))
}

async fn deliver(
    router: &axum::Router,
    signer: Option<&ServiceTokenSigner>,
    body: String,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(organization_events::EVENTS_PATH)
        .header("content-type", "application/json");
    if let Some(s) = signer {
        let signed = s
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::meeting_service(),
            ))
            .unwrap();
        for (k, v) in signed.headers().iter() {
            b = b.header(k, v);
        }
    }
    let r = router
        .clone()
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = r.status();
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn event(
    event_id: Uuid,
    event_type: &str,
    organization: Uuid,
    aggregate_type: &str,
    aggregate: Uuid,
    version: i64,
    payload: Value,
) -> Value {
    json!({
        "eventId": event_id, "eventType": event_type, "schemaVersion": 1,
        "organizationId": organization, "aggregateType": aggregate_type,
        "aggregateId": aggregate, "aggregateVersion": version,
        "occurredAt": "2026-11-09T10:00:00.000000Z",
        "actor": { "kind": "USER", "userId": Uuid::new_v4(), "serviceId": "cfmeet.meeting-service" },
        "correlationId": Uuid::new_v4(), "causationId": Uuid::new_v4(),
        "payload": payload,
    })
}

async fn access(app: &TestApp, org: Uuid, user: Uuid) -> Option<(String, i64)> {
    app.db
        .conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT desired_state, membership_version FROM organization_member_access WHERE organization_id = '{org}' AND user_id = '{user}'"
            ),
        ))
        .await
        .unwrap()
        .map(|r| {
            (
                r.try_get_by_index::<String>(0).unwrap(),
                r.try_get_by_index::<i64>(1).unwrap(),
            )
        })
}

#[tokio::test]
async fn organization_events_are_authenticated_deduplicated_and_applied_in_version_order() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let (orgs, orgs_pub) = signer(ServiceId::organizations(), "organizations-test-1");
    let (dp, dp_pub) = signer(ServiceId::developer_platform(), "developer-platform-test-1");
    let router = organization_events::router(app.state.clone(), verifier(&orgs_pub, &dp_pub));
    let (org, member, membership) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

    // Only a signed Organizations caller with the permission is accepted.
    let removed_id = Uuid::new_v4();
    let removed = event(
        removed_id,
        "membership.removed",
        org,
        "membership",
        membership,
        3,
        json!({ "userId": member, "membershipVersion": 3 }),
    );
    let (s, v) = deliver(&router, None, removed.to_string()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let (s, v) = deliver(&router, Some(&dp), removed.to_string()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "INTERNAL_PERMISSION_DENIED");
    assert_eq!(access(&app, org, member).await, None, "nothing applied");

    // Removal: the member's desired organization access is REMOVED (version 3).
    let (s, v) = deliver(&router, Some(&orgs), removed.to_string()).await;
    assert_eq!((s, v["outcome"].as_str()), (StatusCode::OK, Some("APPLIED")), "{v}");
    assert_eq!(access(&app, org, member).await, Some(("REMOVED".into(), 3)));

    // At-least-once: the same event again is a duplicate (key order does not matter).
    let reordered: Value =
        serde_json::from_str(&serde_json::to_string(&removed).unwrap()).unwrap();
    let (s, v) = deliver(&router, Some(&orgs), reordered.to_string()).await;
    assert_eq!((s, v["outcome"].as_str()), (StatusCode::OK, Some("DUPLICATE")), "{v}");
    // A different body under the same id is refused (the producer dead-letters it).
    let mut forged = removed.clone();
    forged["payload"]["userId"] = json!(Uuid::new_v4());
    let (s, v) = deliver(&router, Some(&orgs), forged.to_string()).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "EVENT_IDENTITY_CONFLICT");

    // An older version of the same membership arriving late is STALE: access stays REMOVED.
    let late = event(
        Uuid::new_v4(),
        "membership.activated",
        org,
        "membership",
        membership,
        2,
        json!({ "userId": member, "role": "MEMBER", "membershipVersion": 2 }),
    );
    let (s, v) = deliver(&router, Some(&orgs), late.to_string()).await;
    assert_eq!((s, v["outcome"].as_str()), (StatusCode::OK, Some("STALE")), "{v}");
    assert_eq!(access(&app, org, member).await, Some(("REMOVED".into(), 3)));

    // A newer version (re-invited and accepted) restores ACTIVE.
    let back = event(
        Uuid::new_v4(),
        "membership.activated",
        org,
        "membership",
        membership,
        4,
        json!({ "userId": member, "role": "MEMBER", "membershipVersion": 4 }),
    );
    let (s, v) = deliver(&router, Some(&orgs), back.to_string()).await;
    assert_eq!((s, v["outcome"].as_str()), (StatusCode::OK, Some("APPLIED")), "{v}");
    assert_eq!(access(&app, org, member).await, Some(("ACTIVE".into(), 4)));

    // Suspension of another member, from a different membership aggregate.
    let other = Uuid::new_v4();
    let (s, _) = deliver(
        &router,
        Some(&orgs),
        event(
            Uuid::new_v4(),
            "membership.suspended",
            org,
            "membership",
            Uuid::new_v4(),
            2,
            json!({ "userId": other, "membershipVersion": 2 }),
        )
        .to_string(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(access(&app, org, other).await, Some(("SUSPENDED".into(), 2)));

    // Legacy authority mode: lifecycle events are recorded (the Meeting Service is itself
    // the organization authority); unknown event types are recorded, never refused.
    for (t, a) in [("organization.suspended", "organization"), ("team.created", "team")] {
        let (s, v) = deliver(
            &router,
            Some(&orgs),
            event(Uuid::new_v4(), t, org, a, Uuid::new_v4(), 1, json!({})).to_string(),
        )
        .await;
        assert_eq!((s, v["outcome"].as_str()), (StatusCode::OK, Some("RECORDED")), "{t}: {v}");
    }

    // Malformed envelopes are refused (permanent: the producer dead-letters them).
    for bad in [
        json!({ "eventId": Uuid::new_v4() }),
        event(Uuid::new_v4(), "membership.removed", org, "membership", Uuid::new_v4(), 1, json!({})),
        {
            let mut e = event(Uuid::new_v4(), "membership.removed", org, "membership", Uuid::new_v4(), 1, json!({ "userId": member }));
            e["schemaVersion"] = json!(2);
            e
        },
    ] {
        let (s, v) = deliver(&router, Some(&orgs), bad.to_string()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR");
    }
    let (s, _) = deliver(&router, Some(&orgs), "x".repeat(70 * 1024)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);

    // Every accepted event is in the inbox exactly once, with its outcome.
    let counts = app
        .db
        .conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT count(*) FILTER (WHERE outcome = 'APPLIED'), count(*) FILTER (WHERE outcome = 'STALE'), count(*) FILTER (WHERE outcome = 'RECORDED'), count(*) FILTER (WHERE event_id = '__ID__') FROM organization_event_inbox"
                .replace("__ID__", &removed_id.to_string()),
        ))
        .await
        .unwrap()
        .unwrap();
    let n = |i| counts.try_get_by_index::<i64>(i).unwrap();
    assert_eq!((n(0), n(1), n(2), n(3)), (3, 1, 2, 1));
}

mod access_worker {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use backend::services::organization_access::{
        self, PortError, PrincipalStatusPort, ScopeEnvironment,
    };

    use super::*;

    #[derive(Default)]
    struct Recording {
        calls: Mutex<Vec<(Uuid, Uuid, String)>>,
        fail: Mutex<Option<PortError>>,
    }

    #[async_trait]
    impl PrincipalStatusPort for Recording {
        async fn set_status(
            &self,
            scope: &ScopeEnvironment,
            user_id: Uuid,
            status: &str,
        ) -> Result<(), PortError> {
            if let Some(e) = self.fail.lock().unwrap().clone() {
                return Err(e);
            }
            self.calls
                .lock()
                .unwrap()
                .push((scope.environment_id, user_id, status.to_string()));
            Ok(())
        }
    }

    /// Lands a newer membership version while the worker is applying the older one.
    struct NewerDuringCall {
        db: sea_orm::DatabaseConnection,
        org: Uuid,
        user: Uuid,
    }

    #[async_trait]
    impl PrincipalStatusPort for NewerDuringCall {
        async fn set_status(&self, _: &ScopeEnvironment, _: Uuid, _: &str) -> Result<(), PortError> {
            self.db
                .execute(Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    format!(
                        "UPDATE organization_member_access SET desired_state = 'ACTIVE', membership_version = 3 WHERE organization_id = '{}' AND user_id = '{}'",
                        self.org, self.user
                    ),
                ))
                .await
                .unwrap();
            Ok(())
        }
    }

    async fn exec(app: &TestApp, sql: String) {
        app.db
            .conn
            .execute(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
            .await
            .unwrap();
    }

    async fn scope_env(app: &TestApp, org: Uuid) -> Uuid {
        let env = Uuid::new_v4();
        exec(
            app,
            format!(
                "INSERT INTO organization_scope_environments (environment_id, organization_id, tenant_id, application_id, service_account_id) \
                 VALUES ('{env}', '{org}', '{}', '{}', '{}')",
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4()
            ),
        )
        .await;
        env
    }

    async fn link(app: &TestApp, user: Uuid, env: Uuid) {
        exec(
            app,
            format!(
                "INSERT INTO communication_principal_links (subject_kind, subject_id, environment_id, principal_id, principal_kind) \
                 VALUES ('USER', '{user}', '{env}', '{}', 'NATIVE_CF_USER')",
                Uuid::new_v4()
            ),
        )
        .await;
    }

    async fn desire(app: &TestApp, org: Uuid, user: Uuid, desired: &str, version: i64) {
        exec(
            app,
            format!(
                "INSERT INTO organization_member_access (organization_id, user_id, desired_state, membership_version) VALUES ('{org}', '{user}', '{desired}', {version}) \
                 ON CONFLICT (organization_id, user_id) DO UPDATE SET desired_state = EXCLUDED.desired_state, membership_version = EXCLUDED.membership_version, next_attempt_at = now()"
            ),
        )
        .await;
    }

    async fn row(
        app: &TestApp,
        org: Uuid,
        user: Uuid,
    ) -> (Option<String>, Option<i64>, i32, Option<String>) {
        let r = app
            .db
            .conn
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT applied_state, applied_version, attempts, last_error_class FROM organization_member_access WHERE organization_id = '{org}' AND user_id = '{user}'"),
            ))
            .await
            .unwrap()
            .unwrap();
        (
            r.try_get_by_index(0).unwrap(),
            r.try_get_by_index(1).unwrap(),
            r.try_get_by_index(2).unwrap(),
            r.try_get_by_index(3).unwrap(),
        )
    }

    #[tokio::test]
    async fn offboarding_suspends_only_organization_scope_principals_and_restores_on_readmission() {
        let Some(app) = TestApp::new().await else {
            return;
        };
        let member = Uuid::new_v4();
        let (org, other_org) = (Uuid::new_v4(), Uuid::new_v4());
        // Principals in this organization's environment, another organization's
        // environment and the personal environment (not an organization scope).
        let org_env = scope_env(&app, org).await;
        let other_env = scope_env(&app, other_org).await;
        let personal_env = Uuid::new_v4();
        for env in [org_env, other_env, personal_env] {
            link(&app, member, env).await;
        }
        desire(&app, org, member, "REMOVED", 3).await;

        // An outage is retried with backoff, never dropped.
        let port = Recording::default();
        *port.fail.lock().unwrap() = Some(PortError {
            class: "PLATFORM_ACCESS_503".into(),
            permanent: false,
        });
        let r = organization_access::run_once(&app.db.conn, &port, 10).await.unwrap();
        assert_eq!((r.applied, r.failed), (0, 1));
        assert_eq!(
            row(&app, org, member).await,
            (None, None, 1, Some("PLATFORM_ACCESS_503".into()))
        );
        let backlog = organization_access::backlog(&app.db.conn).await.unwrap();
        assert_eq!((backlog.pending, backlog.failing), (1, 1));
        // Not due again until the backoff elapses.
        *port.fail.lock().unwrap() = None;
        assert_eq!(
            organization_access::run_once(&app.db.conn, &port, 10).await.unwrap(),
            Default::default()
        );
        exec(&app, "UPDATE organization_member_access SET next_attempt_at = now()".into()).await;

        // Removal suspends the principal of THIS organization only.
        let r = organization_access::run_once(&app.db.conn, &port, 10).await.unwrap();
        assert_eq!((r.applied, r.principals_updated), (1, 1));
        assert_eq!(
            port.calls.lock().unwrap().clone(),
            vec![(org_env, member, "SUSPENDED".to_string())],
            "never the other organization's or the personal principal"
        );
        assert_eq!(
            row(&app, org, member).await,
            (Some("REMOVED".into()), Some(3), 0, None)
        );
        assert_eq!(organization_access::backlog(&app.db.conn).await.unwrap().pending, 0);
        assert_eq!(
            organization_access::run_once(&app.db.conn, &port, 10).await.unwrap(),
            Default::default(),
            "nothing more to do"
        );

        // Re-admission (a newer membership version) restores the principal.
        desire(&app, org, member, "ACTIVE", 5).await;
        organization_access::run_once(&app.db.conn, &port, 10).await.unwrap();
        assert_eq!(port.calls.lock().unwrap().last().unwrap().2, "ACTIVE");
        assert_eq!(row(&app, org, member).await.0.as_deref(), Some("ACTIVE"));

        // A member without an organization-scope principal is applied without a call.
        let stranger = Uuid::new_v4();
        desire(&app, org, stranger, "SUSPENDED", 2).await;
        let before = port.calls.lock().unwrap().len();
        let r = organization_access::run_once(&app.db.conn, &port, 10).await.unwrap();
        assert_eq!((r.applied, r.principals_updated), (1, 0));
        assert_eq!(port.calls.lock().unwrap().len(), before);
    }

    #[tokio::test]
    async fn a_newer_version_arriving_mid_pass_stays_due() {
        let Some(app) = TestApp::new().await else {
            return;
        };
        let (org, user) = (Uuid::new_v4(), Uuid::new_v4());
        let env = scope_env(&app, org).await;
        link(&app, user, env).await;
        desire(&app, org, user, "SUSPENDED", 2).await;
        let racing = NewerDuringCall {
            db: app.db.conn.clone(),
            org,
            user,
        };
        organization_access::run_once(&app.db.conn, &racing, 10)
            .await
            .unwrap();
        let (applied, version, _, _) = row(&app, org, user).await;
        assert_eq!(
            (applied, version),
            (None, None),
            "version 2 is not recorded as the applied state of version 3"
        );
        assert_eq!(organization_access::backlog(&app.db.conn).await.unwrap().pending, 1);
    }
}
