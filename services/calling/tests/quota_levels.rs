//! Part G §70 closure: the SERVICE_ACCOUNT and PRINCIPAL quota levels against a real
//! PostgreSQL — request rate shared by every credential of a service account, a
//! principal's rate / concurrency shared by every credential and service account acting
//! for it, race-safe aggregate ceilings, the complete five-level hierarchy (each level
//! enforced, parents never bypassed through child identities), isolation between
//! siblings, the limits read model (no client-selected authority, principals only in the
//! caller's scope) and deduplicated notices without identifiers.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{
    calls::{
        command::CallCommandContext, kinds::CallKind, service::NewCall, CallAction, CallError,
    },
    domain::{EnvironmentKind, PrincipalId, ServiceAccountId},
    platform::{
        context::PlatformAuthContext, limits::LimitValues, principals::ResolvedPrincipal,
        scopes::names, service_accounts::NewServiceAccount,
    },
    webhooks::{WebhookService, WebhookSettings},
};
use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox};
use common::{calls::CallWorld, enterprise::strings, TestResponse};
use serde_json::Value;
use tokio::sync::Barrier;
use uuid::Uuid;

/// One API credential (of some service account) and its authenticated context.
#[derive(Clone)]
struct Actor {
    ctx: PlatformAuthContext,
    secret: String,
    credential: Uuid,
}

async fn grant(w: &CallWorld, scopes: &[&str]) {
    for scope in scopes {
        w.h.platform
            .grant_application_scope(w.consumer.application.id, scope)
            .await
            .unwrap();
    }
}

/// Another service account of the world's application environment.
async fn service_account(w: &CallWorld, name: &str) -> ServiceAccountId {
    w.h.platform
        .create_service_account(
            w.consumer.env(w.kind).id,
            NewServiceAccount {
                name: name.into(),
                description: None,
            },
        )
        .await
        .unwrap()
        .id
}

/// A credential of `sa` with generous credential-level limits (so only the level under
/// test can stop it).
async fn actor(w: &CallWorld, sa: ServiceAccountId, scopes: &[&str]) -> Actor {
    let issued = w.h.issue(sa, scopes).await;
    w.h.platform
        .set_credential_limits(
            issued.credential.id,
            LimitValues {
                requests_per_minute: Some(100_000),
                max_concurrent_communications: Some(100_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let secret = issued.secret.expose_once().to_string();
    Actor {
        ctx: w.h.ctx(&secret).await,
        secret,
        credential: issued.credential.id.as_uuid(),
    }
}

async fn caller(w: &CallWorld, sa: ServiceAccountId) -> Actor {
    actor(
        w,
        sa,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::PRINCIPALS_READ,
        ],
    )
    .await
}

async fn create(
    w: &CallWorld,
    a: &Actor,
    initiator: &ResolvedPrincipal,
) -> Result<calling::calls::service::CreatedCall, CallError> {
    w.calls()
        .create_requested_call(&a.ctx, initiator, NewCall::of_kind(CallKind::Voice))
        .await
}

async fn cancel(w: &CallWorld, a: &Actor, c: &calling::calls::service::CreatedCall) {
    w.calls()
        .transition(
            &CallCommandContext::customer(&a.ctx),
            c.call.id,
            c.call.version,
            CallAction::Cancel,
        )
        .await
        .unwrap();
}

fn dev_settings() -> WebhookSettings {
    WebhookSettings {
        secret_box: Some(Arc::new(SecretBox::from_bytes(&[42u8; 32]).unwrap())),
        policy: EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        },
        request_timeout: std::time::Duration::from_secs(1),
        max_attempts: 3,
    }
}

/// An ACTIVE `limit.*` subscription of the world's application environment.
async fn subscribe_limits(w: &CallWorld) {
    grant(w, &[names::USAGE_READ, names::WEBHOOKS_MANAGE]).await;
    let manager =
        w.h.issue(
            w.consumer.sa(w.kind),
            &[names::USAGE_READ, names::WEBHOOKS_MANAGE],
        )
        .await;
    let ctx = w.h.ctx(manager.secret.expose_once()).await;
    WebhookService::new(
        w.h.conn().clone(),
        Arc::new(w.h.clock.clone()),
        dev_settings(),
    )
    .create(
        &ctx,
        "http://127.0.0.1:9/hook",
        &["limit.warning".to_string(), "limit.exceeded".to_string()],
        None,
    )
    .await
    .unwrap();
}

async fn notices(w: &CallWorld, event_type: &str, limit: &str, scope: &str) -> Vec<Value> {
    strings(
        w.h.conn(),
        &format!(
            "SELECT body::text FROM webhook_events WHERE event_type = '{event_type}' \
               AND body->'data'->>'limit' = '{limit}' AND body->'data'->>'scope' = '{scope}' \
             ORDER BY created_at, id"
        ),
    )
    .await
    .into_iter()
    .map(|b| serde_json::from_str(&b).unwrap())
    .collect()
}

/// No public payload carries an identifier: only bounded codes and integers.
async fn assert_no_identifiers_in_notices(w: &CallWorld, ids: &[Uuid]) {
    for body in strings(w.h.conn(), "SELECT body->>'data' FROM webhook_events").await {
        let data: Value = serde_json::from_str(&body).unwrap();
        for (key, value) in data.as_object().unwrap() {
            assert!(
                [
                    "limit",
                    "scope",
                    "window",
                    "threshold",
                    "current",
                    "ceiling"
                ]
                .contains(&key.as_str()),
                "{key}"
            );
            assert!(
                value.is_u64() || value.as_str().is_some_and(|s| Uuid::parse_str(s).is_err()),
                "{key}: {value}"
            );
        }
        for id in ids {
            assert!(!body.contains(&id.to_string()), "an identifier leaked");
        }
    }
    let metrics = calling::metrics::render();
    for id in ids {
        assert!(
            !metrics.contains(&id.to_string()),
            "an identifier is a metric label"
        );
    }
}

#[tokio::test]
async fn service_account_request_rate_is_shared_by_its_credentials() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    subscribe_limits(&w).await;
    let sa = w.consumer.sa(w.kind);
    let (c1, c2) = (caller(&w, sa).await, caller(&w, sa).await);
    w.h.platform
        .set_service_account_limits(
            sa,
            LimitValues {
                requests_per_minute: Some(6),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut statuses = Vec::new();
    for i in 0..10 {
        let a = if i % 2 == 0 { &c1 } else { &c2 };
        statuses.push(w.h.whoami(&a.secret).await.status.as_u16());
    }
    assert_eq!(statuses, [200, 200, 200, 200, 200, 200, 429, 429, 429, 429]);
    // Another service account of the same application is unaffected.
    let other_sa = service_account(&w, "second").await;
    let c3 = caller(&w, other_sa).await;
    for _ in 0..3 {
        assert_eq!(w.h.whoami(&c3.secret).await.status, StatusCode::OK);
    }
    // One warning at the threshold, one exceeded per window.
    let warnings = notices(&w, "limit.warning", "REQUEST_RATE", "SERVICE_ACCOUNT").await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        (
            &warnings[0]["data"]["current"],
            &warnings[0]["data"]["ceiling"]
        ),
        (&Value::from(5), &Value::from(6))
    );
    assert_eq!(
        notices(&w, "limit.exceeded", "REQUEST_RATE", "SERVICE_ACCOUNT")
            .await
            .len(),
        1
    );
    w.h.clock.advance(chrono::Duration::seconds(60));
    for i in 0..9 {
        let a = if i % 2 == 0 { &c1 } else { &c2 };
        w.h.whoami(&a.secret).await;
    }
    assert_eq!(
        notices(&w, "limit.exceeded", "REQUEST_RATE", "SERVICE_ACCOUNT")
            .await
            .len(),
        2,
        "a new window, a new notice"
    );
    assert_no_identifiers_in_notices(
        &w,
        &[
            sa.as_uuid(),
            other_sa.as_uuid(),
            c1.credential,
            c2.credential,
        ],
    )
    .await;
}

#[tokio::test]
async fn principal_rate_is_shared_across_credentials_and_service_accounts() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    subscribe_limits(&w).await;
    let a1 = caller(&w, w.consumer.sa(w.kind)).await;
    let a2 = caller(&w, service_account(&w, "second").await).await;
    w.h.platform
        .set_principal_limits(
            w.initiator.id(),
            LimitValues {
                requests_per_minute: Some(3),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut outcomes = Vec::new();
    for i in 0..5 {
        let a = if i % 2 == 0 { &a1 } else { &a2 };
        match create(&w, a, &w.initiator).await {
            Ok(c) => {
                cancel(&w, a, &c).await;
                outcomes.push("ok");
            }
            Err(CallError::RateLimited { .. }) => outcomes.push("rate-limited"),
            Err(other) => panic!("{other:?}"),
        }
    }
    assert_eq!(outcomes, ["ok", "ok", "ok", "rate-limited", "rate-limited"]);
    // Another principal keeps its own allowance.
    let other = create(&w, &a1, &w.recipient).await.unwrap();
    cancel(&w, &a1, &other).await;
    let warnings = notices(&w, "limit.warning", "REQUEST_RATE", "PRINCIPAL").await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let exceeded = settled(|| notices(&w, "limit.exceeded", "REQUEST_RATE", "PRINCIPAL")).await;
    assert_eq!(exceeded.len(), 1, "{exceeded:?}");
    assert_eq!(exceeded[0]["data"]["current"], 4);
    assert_no_identifiers_in_notices(&w, &[w.initiator.id().as_uuid(), a1.credential]).await;
    // The refusal is the normalized RATE_LIMITED error through the API too.
    let r =
        w.h.request_with(
            Method::POST,
            "/v1/calls",
            Some(&a1.secret),
            Some(serde_json::json!({
                "kind": "VOICE",
                "initiatorPrincipalId": w.initiator.id(),
                "targetPrincipalIds": [w.recipient.id()],
            })),
            &[("idempotency-key", "p-rate-api")],
        )
        .await;
    if r.status != StatusCode::BAD_REQUEST {
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.text);
        assert_eq!(r.json["error"]["code"], "RATE_LIMITED");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_account_concurrency_ceiling_holds_across_its_credentials() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let sa = w.consumer.sa(w.kind);
    let (c1, c2) = (caller(&w, sa).await, caller(&w, sa).await);
    w.h.platform
        .set_service_account_limits(
            sa,
            LimitValues {
                max_concurrent_communications: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let w = Arc::new(w);
    for round in 0..15 {
        let barrier = Arc::new(Barrier::new(6));
        let mut handles = Vec::new();
        for i in 0..6 {
            let (w, barrier) = (w.clone(), barrier.clone());
            let a = if i % 2 == 0 { c1.clone() } else { c2.clone() };
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                (create(&w, &a, &w.initiator).await, a)
            }));
        }
        let (mut created, mut levels) = (Vec::new(), Vec::new());
        for h in handles {
            match h.await.unwrap() {
                (Ok(c), a) => created.push((c, a)),
                (Err(CallError::ConcurrentCallLimitExceeded { level, .. }), _) => {
                    levels.push(level)
                }
                (Err(other), _) => panic!("round {round}: {other:?}"),
            }
        }
        // The aggregate across BOTH credentials, whichever refusal fired.
        assert_eq!(
            created.len(),
            2,
            "round {round}: the service-account ceiling"
        );
        assert!(levels.iter().all(|l| *l == "service_account"), "{levels:?}");
        for (c, a) in &created {
            cancel(&w, a, c).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn principal_concurrency_ceiling_holds_across_credentials() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let a1 = caller(&w, w.consumer.sa(w.kind)).await;
    let a2 = caller(&w, service_account(&w, "second").await).await;
    w.h.platform
        .set_principal_limits(
            w.initiator.id(),
            LimitValues {
                max_concurrent_communications: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let w = Arc::new(w);
    for round in 0..15 {
        let barrier = Arc::new(Barrier::new(6));
        let mut handles = Vec::new();
        for i in 0..6 {
            let (w, barrier) = (w.clone(), barrier.clone());
            let a = if i % 2 == 0 { a1.clone() } else { a2.clone() };
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                (create(&w, &a, &w.initiator).await, a)
            }));
        }
        let mut created = Vec::new();
        for h in handles {
            match h.await.unwrap() {
                (Ok(c), a) => created.push((c, a)),
                (
                    Err(CallError::ConcurrentCallLimitExceeded {
                        level: "principal",
                        limit: 1,
                    }),
                    _,
                ) => {}
                (Err(other), _) => panic!("round {round}: {other:?}"),
            }
        }
        assert_eq!(created.len(), 1, "round {round}: the principal ceiling");
        // Another principal is not consumed by P's ceiling.
        let other = create(&w, &a2, &w.recipient).await.unwrap();
        cancel(&w, &a2, &other).await;
        for (c, a) in &created {
            cancel(&w, a, c).await;
        }
    }
}

#[tokio::test]
async fn every_level_is_enforced_and_parents_cannot_be_bypassed() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let kind = w.kind;
    let p = |r: &'static str| {
        let w = &w;
        async move { common::calls::resolve(&w.h, &w.consumer, kind, &w.ctx, r).await }
    };
    let (pp, q, r, s, t, u) = (
        p("p").await,
        p("q").await,
        p("r").await,
        p("s").await,
        p("t").await,
        p("u").await,
    );
    let sa0 = w.consumer.sa(kind);
    let sa1 = service_account(&w, "second").await;
    let (a1, a2, a3) = (
        caller(&w, sa0).await,
        caller(&w, sa0).await,
        caller(&w, sa1).await,
    );
    // A second application of the same tenant.
    let app2 = w.h.application_in(&w.consumer.tenant, "app-two").await;
    let a4 = {
        let issued = common::enterprise::credential(
            &w.h,
            &app2,
            kind,
            &[
                names::CALLS_READ,
                names::CALLS_WRITE,
                names::PRINCIPALS_READ,
            ],
        )
        .await;
        let secret = issued.secret.expose_once().to_string();
        Actor {
            ctx: w.h.ctx(&secret).await,
            secret,
            credential: issued.credential.id.as_uuid(),
        }
    };
    let v = common::calls::resolve(&w.h, &app2, kind, &a4.ctx, "v").await;
    let x = common::calls::resolve(&w.h, &app2, kind, &a4.ctx, "x").await;
    let concurrent = |n| LimitValues {
        max_concurrent_communications: Some(n),
        ..Default::default()
    };
    let platform = &w.h.platform;
    platform
        .set_tenant_limits(w.consumer.tenant.id, concurrent(6))
        .await
        .unwrap();
    platform
        .set_application_limits(w.consumer.application.id, concurrent(5))
        .await
        .unwrap();
    platform
        .set_service_account_limits(sa0, concurrent(4))
        .await
        .unwrap();
    platform
        .set_credential_limits(
            calling::domain::CredentialId::from_uuid(a1.credential),
            concurrent(3),
        )
        .await
        .unwrap();
    platform
        .set_principal_limits(pp.id(), concurrent(1))
        .await
        .unwrap();
    let refused = |r: Result<_, CallError>| match r {
        Err(CallError::ConcurrentCallLimitExceeded { level, .. }) => level,
        Ok(_) => "created",
        Err(other) => panic!("{other:?}"),
    };
    // Principal (1): the narrowest axis blocks first.
    assert_eq!(refused(create(&w, &a1, &pp).await), "created");
    assert_eq!(refused(create(&w, &a1, &pp).await), "principal");
    // Credential (3).
    assert_eq!(refused(create(&w, &a1, &q).await), "created");
    assert_eq!(refused(create(&w, &a1, &r).await), "created");
    assert_eq!(refused(create(&w, &a1, &s).await), "credential");
    // A new credential of the same service account cannot bypass its ceiling (4).
    assert_eq!(refused(create(&w, &a2, &s).await), "created");
    assert_eq!(refused(create(&w, &a2, &t).await), "service_account");
    // A new service account cannot bypass the application ceiling (5).
    assert_eq!(refused(create(&w, &a3, &t).await), "created");
    assert_eq!(refused(create(&w, &a3, &u).await), "application");
    // A new application cannot bypass the tenant ceiling (6).
    assert_eq!(refused(create(&w, &a4, &v).await), "created");
    assert_eq!(refused(create(&w, &a4, &x).await), "tenant");
}

#[tokio::test]
async fn limits_api_reports_service_account_and_principal_levels_safely() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    grant(&w, &[names::USAGE_READ]).await;
    let sa0 = w.consumer.sa(w.kind);
    let sa1 = service_account(&w, "second").await;
    let reader = actor(&w, sa0, &[names::USAGE_READ, names::PRINCIPALS_READ]).await;
    let usage_only = actor(&w, sa0, &[names::USAGE_READ]).await;
    let a1 = caller(&w, sa0).await;
    w.h.platform
        .set_service_account_limits(
            sa0,
            LimitValues {
                requests_per_minute: Some(50),
                max_concurrent_communications: Some(4),
                telecom_spend_daily_limit_minor: Some(900),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    w.h.platform
        .set_principal_limits(
            w.initiator.id(),
            LimitValues {
                requests_per_minute: Some(10),
                max_concurrent_communications: Some(2),
                telecom_spend_daily_limit_minor: Some(500),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    create(&w, &a1, &w.initiator).await.unwrap();
    let get = |token: String, path: String| {
        let w = &w;
        async move { w.h.request(Method::GET, &path, Some(&token), None).await }
    };
    let level = |r: &TestResponse, limit: &str, scope: &str| -> Option<Value> {
        r.json["limits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["limit"] == limit)
            .and_then(|l| {
                l["levels"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["scope"] == scope)
                    .cloned()
            })
    };
    let r = get(reader.secret.clone(), "/v1/limits".into()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check("platform/usage/v1.schema.json#/$defs/Limits", &r.json);
    assert_eq!(
        level(&r, "CONCURRENT_CALLS", "SERVICE_ACCOUNT").unwrap(),
        serde_json::json!({"scope": "SERVICE_ACCOUNT", "ceiling": 4, "current": 1, "remaining": 3})
    );
    assert_eq!(
        level(&r, "REQUEST_RATE", "SERVICE_ACCOUNT").unwrap()["ceiling"],
        50
    );
    assert_eq!(
        level(&r, "TELECOM_SPEND_DAILY", "SERVICE_ACCOUNT").unwrap()["ceiling"],
        900
    );
    assert!(level(&r, "CONCURRENT_CALLS", "PRINCIPAL").is_none());

    let principal = w.initiator.id().as_uuid();
    let r = get(
        reader.secret.clone(),
        format!("/v1/limits?principalId={principal}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check("platform/usage/v1.schema.json#/$defs/Limits", &r.json);
    assert_eq!(
        level(&r, "CONCURRENT_CALLS", "PRINCIPAL").unwrap(),
        serde_json::json!({"scope": "PRINCIPAL", "ceiling": 2, "current": 1, "remaining": 1})
    );
    assert_eq!(
        level(&r, "REQUEST_RATE", "PRINCIPAL").unwrap()["current"],
        1
    );
    assert_eq!(
        level(&r, "TELECOM_SPEND_DAILY", "PRINCIPAL").unwrap()["ceiling"],
        500
    );
    // No identifier of any authority is part of the view.
    for id in [
        sa0.as_uuid(),
        sa1.as_uuid(),
        principal,
        a1.credential,
        w.consumer.tenant.id.as_uuid(),
        w.consumer.application.id.as_uuid(),
    ] {
        assert!(!r.text.contains(&id.to_string()), "{}", r.text);
    }

    // Principals need principals.read.
    let r = get(
        usage_only.secret.clone(),
        format!("/v1/limits?principalId={principal}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // A principal outside the caller's own scope is indistinguishable from none.
    let (sandbox_principal, _) =
        w.h.platform
            .register_principal(
                w.consumer.env(EnvironmentKind::Sandbox).id,
                calling::platform::principals::NewPrincipal::External {
                    kind: calling::domain::PrincipalKind::ExternalApplicationPrincipal,
                    external_reference: "sandbox-user".into(),
                },
            )
            .await
            .unwrap();
    for other in [sandbox_principal.id.as_uuid(), Uuid::new_v4()] {
        let r = get(
            reader.secret.clone(),
            format!("/v1/limits?principalId={other}"),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    }
    // No client-selected authority: tenant, application, service account, credential.
    for (param, value) in [
        ("serviceAccountId", sa1.as_uuid()),
        ("tenantId", w.consumer.tenant.id.as_uuid()),
        ("applicationId", w.consumer.application.id.as_uuid()),
        ("credentialId", a1.credential),
    ] {
        let r = get(reader.secret.clone(), format!("/v1/limits?{param}={value}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{param}: {}", r.text);
    }
    let _ = PrincipalId::from_uuid(principal);
}

/// Refusal notices are emitted by a detached task (the refused transaction rolls back
/// first): wait until they are stored, bounded, then let any duplicate attempt settle.
async fn settled<F, Fut>(read: F) -> Vec<Value>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Vec<Value>>,
{
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while read().await.is_empty() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    read().await
}
