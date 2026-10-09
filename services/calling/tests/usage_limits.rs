//! Part G §69 / §70 against a real PostgreSQL: the usage ledger API (final facts of the
//! caller's application environment only, filters, keyset pagination, no provider
//! data, constraints that keep cost evidence honest and facts immutable), the limits
//! API (every level of the precedence chain and the binding one), the tenant ceiling under
//! concurrent creations across applications, and deduplicated limit notices.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{
    calls::{
        command::CallCommandContext, kinds::CallKind, service::NewCall, CallAction, CallError,
    },
    domain::EnvironmentKind,
    platform::{limits::LimitValues, scopes::names},
    webhooks::{WebhookService, WebhookSettings},
};
use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use common::{calls::CallWorld, enterprise::strings, exec, scalar_i64, Harness, TestResponse};
use serde_json::{json, Value};
use tokio::sync::Barrier;
use uuid::Uuid;

async fn grant(w: &CallWorld, scopes: &[&str]) {
    for scope in scopes {
        w.h.platform
            .grant_application_scope(w.consumer.application.id, scope)
            .await
            .unwrap();
    }
}

async fn get(w: &CallWorld, token: &str, path: &str) -> TestResponse {
    w.h.request(Method::GET, path, Some(token), None).await
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A FINAL usage fact written as its writer would (scope copied from the Call).
async fn fact(
    w: &CallWorld,
    call: Uuid,
    usage_type: &str,
    key: &str,
    at: DateTime<Utc>,
    finalized: bool,
    extra: &str,
) {
    let (cols, vals) = match extra {
        "" => (String::new(), String::new()),
        e => {
            let (c, v) = e.split_once('=').unwrap();
            (format!(", {c}"), format!(", {v}"))
        }
    };
    exec(
        w.h.conn(),
        &format!(
            "INSERT INTO call_usage_records (id, call_id, tenant_id, application_id, environment_id, \
               usage_type, quantity, unit, idempotency_key, finalized_at, created_at, updated_at{cols}) \
             SELECT gen_random_uuid(), id, tenant_id, application_id, environment_id, '{usage_type}', \
               1500, 'MILLISECONDS', '{key}', {fin}, '{at}', '{at}'{vals} FROM calls WHERE id = '{call}'",
            at = iso(at),
            fin = if finalized {
                format!("'{}'", iso(at))
            } else {
                "NULL".into()
            },
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn usage_api_lists_final_facts_of_the_caller_scope_only() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    grant(&w, &[names::USAGE_READ]).await;
    let reader = w.h.issue(w.consumer.sa(w.kind), &[names::USAGE_READ]).await;
    let reader = reader.secret.expose_once().to_string();
    let sandbox =
        w.h.issue(
            w.consumer.sa(EnvironmentKind::Sandbox),
            &[names::USAGE_READ],
        )
        .await;
    let sandbox = sandbox.secret.expose_once().to_string();
    let (c1, c2, c3) = (w.create().await, w.create().await, w.create().await);
    let (c1, c2, c3) = (
        c1.call.id.as_uuid(),
        c2.call.id.as_uuid(),
        c3.call.id.as_uuid(),
    );
    let t = common::t0() + Duration::minutes(5);
    fact(&w, c1, "CALL_CONNECTED_TIME", "call", t, true, "").await;
    fact(
        &w,
        c2,
        "RECORDING_ARTIFACT",
        "recording-artifact:a",
        t + Duration::seconds(1),
        true,
        "size_bytes=4096",
    )
    .await;
    fact(
        &w,
        c1,
        "VOICEMAIL_ARTIFACT",
        "voicemail-artifact:b",
        t + Duration::seconds(2),
        true,
        "size_bytes=2048",
    )
    .await;
    // Not final yet: never listed.
    fact(&w, c3, "CALL_CONNECTED_TIME", "call", t, false, "").await;

    // Authority: usage.read only.
    let r = get(&w, w.secret(), "/v1/usage").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);

    let r = get(&w, &reader, "/v1/usage").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check("platform/usage/v1.schema.json#/$defs/UsageList", &r.json);
    let records = r.json["data"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(
        records
            .iter()
            .map(|r| r["usageType"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "CALL_CONNECTED_TIME",
            "RECORDING_ARTIFACT",
            "VOICEMAIL_ARTIFACT"
        ]
    );
    assert_eq!(records[1]["resourceClass"], "STORAGE");
    assert_eq!(records[1]["sizeBytes"], 4096);
    assert_eq!(records[0]["resourceClass"], "APP_MEDIA");
    for forbidden in [
        "provider",
        "objectKey",
        "object_key",
        "phone",
        "sip:",
        "+234",
    ] {
        assert!(!r.text.contains(forbidden), "{forbidden}: {}", r.text);
    }

    // Filters.
    let count = |path: String| {
        let (w, reader) = (&w, reader.clone());
        async move {
            let r = get(w, &reader, &path).await;
            assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text);
            r.json["data"].as_array().unwrap().len()
        }
    };
    assert_eq!(count("/v1/usage?resourceClass=STORAGE".into()).await, 2);
    assert_eq!(
        count("/v1/usage?usageType=CALL_CONNECTED_TIME".into()).await,
        1
    );
    assert_eq!(
        count("/v1/usage?usageType=CALL_CONNECTED_TIME&resourceClass=STORAGE".into()).await,
        0
    );
    assert_eq!(
        count(format!("/v1/usage?from={}", iso(t + Duration::seconds(1)))).await,
        2
    );
    assert_eq!(
        count(format!("/v1/usage?to={}", iso(t + Duration::seconds(1)))).await,
        1
    );
    for bad in [
        format!(
            "/v1/usage?from={}&to={}",
            iso(t),
            iso(t - Duration::seconds(1))
        ),
        "/v1/usage?usageType=MINUTES".into(),
        "/v1/usage?resourceClass=BILLING".into(),
        "/v1/usage?limit=0".into(),
        "/v1/usage?tenantId=x".into(),
        "/v1/usage?from=yesterday".into(),
    ] {
        let r = get(&w, &reader, &bad).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
    }

    // Keyset pagination.
    let first = get(&w, &reader, "/v1/usage?limit=2").await;
    assert_eq!(first.json["data"].as_array().unwrap().len(), 2);
    let cursor = first.json["nextCursor"].as_str().unwrap().to_string();
    let second = get(&w, &reader, &format!("/v1/usage?limit=2&cursor={cursor}")).await;
    assert_eq!(second.json["data"].as_array().unwrap().len(), 1);
    assert!(second.json["nextCursor"].is_null());
    assert_eq!(second.json["data"][0]["usageType"], "VOICEMAIL_ARTIFACT");

    // Another environment of the same application sees nothing.
    let r = get(&w, &sandbox, "/v1/usage").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["data"], json!([]));

    // Honest facts at the database: an UNKNOWN cost has no amount, a fact is immutable,
    // an artifact has a size, a duplicate key is refused.
    let refused = |extra: &'static str, key: &'static str, usage_type: &'static str| {
        let w = &w;
        async move {
            let sql = format!(
                "INSERT INTO call_usage_records (id, call_id, tenant_id, application_id, environment_id, \
                   usage_type, quantity, unit, idempotency_key, created_at, updated_at{}) \
                 SELECT gen_random_uuid(), id, tenant_id, application_id, environment_id, '{usage_type}', \
                   1, 'MILLISECONDS', '{key}', now(), now(){} FROM calls WHERE id = '{c1}'",
                extra.split_once('=').map(|(c, _)| format!(", {c}")).unwrap_or_default(),
                extra.split_once('=').map(|(_, v)| format!(", {v}")).unwrap_or_default(),
            );
            exec(w.h.conn(), &sql).await.unwrap_err().to_string()
        }
    };
    assert!(refused(
        "cost_status, cost_minor='UNKNOWN', 0",
        "x1",
        "CALL_CONNECTED_TIME"
    )
    .await
    .contains("ck_call_usage_records_cost"));
    assert!(refused("", "x2", "RECORDING_ARTIFACT")
        .await
        .contains("ck_call_usage_records_artifact"));
    assert!(refused("", "call", "CALL_CONNECTED_TIME")
        .await
        .contains("uq_call_usage_records_idempotency"));
    assert!(exec(
        w.h.conn(),
        &format!("UPDATE call_usage_records SET size_bytes = 1 WHERE call_id = '{c2}'"),
    )
    .await
    .is_err());
}

#[tokio::test]
async fn limits_api_reports_every_level_and_the_binding_one() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    grant(&w, &[names::USAGE_READ]).await;
    w.h.platform
        .set_tenant_limits(
            w.consumer.tenant.id,
            LimitValues {
                requests_per_minute: Some(1_000),
                max_concurrent_communications: Some(3),
                telecom_spend_daily_limit_minor: Some(5_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    w.h.platform
        .set_application_limits(
            w.consumer.application.id,
            LimitValues {
                max_concurrent_communications: Some(10),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    w.create().await;
    w.create().await;
    let reader = w.h.issue(w.consumer.sa(w.kind), &[names::USAGE_READ]).await;
    let r = get(&w, reader.secret.expose_once(), "/v1/limits").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check("platform/usage/v1.schema.json#/$defs/Limits", &r.json);
    let limit = |name: &str| -> Value {
        r.json["limits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["limit"] == name)
            .cloned()
            .unwrap()
    };
    let concurrent = limit("CONCURRENT_CALLS");
    assert_eq!(
        concurrent["levels"],
        json!([
            {"scope": "TENANT", "ceiling": 3, "current": 2, "remaining": 1},
            {"scope": "APPLICATION", "ceiling": 10, "current": 2, "remaining": 8},
            {"scope": "CREDENTIAL", "ceiling": 3, "current": 0, "remaining": 3},
        ])
    );
    assert_eq!(
        (&concurrent["scope"], &concurrent["remaining"]),
        (&json!("TENANT"), &json!(1))
    );
    let rate = limit("REQUEST_RATE");
    assert_eq!(rate["window"], "MINUTE");
    assert_eq!(rate["levels"][0]["scope"], "TENANT");
    assert!(rate["levels"][0]["current"].as_i64().unwrap() >= 1);
    // Effective per credential: the configured tenant level (a configured level replaces
    // the platform default).
    assert_eq!(rate["levels"][1]["scope"], "CREDENTIAL");
    assert_eq!(rate["levels"][1]["ceiling"], 1_000);
    // No spend policy for the environment: no telecom spend at all (fail closed).
    let daily = limit("TELECOM_SPEND_DAILY");
    assert_eq!(
        (&daily["scope"], &daily["ceiling"]),
        (&json!("ENVIRONMENT"), &json!(0))
    );
    assert_eq!(daily["levels"][1]["scope"], "TENANT");
    assert_eq!(daily["levels"][1]["ceiling"], 5_000);
    assert!(daily["currency"].as_str().unwrap().len() == 3);

    let r = get(&w, w.secret(), "/v1/limits").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenant_concurrency_ceiling_holds_across_applications() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let w = CallWorld::in_harness(h, "acme", EnvironmentKind::Production).await;
    // A second application of the same tenant, its own credential and principal.
    let second = w.h.application_in(&w.consumer.tenant, "second").await;
    let token = common::enterprise::credential(
        &w.h,
        &second,
        EnvironmentKind::Production,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::PRINCIPALS_READ,
        ],
    )
    .await;
    let ctx2 = w.h.ctx(token.secret.expose_once()).await;
    let initiator2 =
        common::calls::resolve(&w.h, &second, EnvironmentKind::Production, &ctx2, "init-2").await;
    w.h.platform
        .set_tenant_limits(
            w.consumer.tenant.id,
            LimitValues {
                max_concurrent_communications: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let w = Arc::new(w);
    let (ctx2, initiator2) = (Arc::new(ctx2), Arc::new(initiator2));
    for round in 0..15 {
        let barrier = Arc::new(Barrier::new(6));
        let mut handles = Vec::new();
        for i in 0..6 {
            let (w, barrier, ctx2, initiator2) =
                (w.clone(), barrier.clone(), ctx2.clone(), initiator2.clone());
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let new = NewCall::of_kind(CallKind::Voice);
                if i % 2 == 0 {
                    w.calls()
                        .create_requested_call(&w.ctx, &w.initiator, new)
                        .await
                } else {
                    w.calls()
                        .create_requested_call(&ctx2, &initiator2, new)
                        .await
                }
            }));
        }
        let mut created = Vec::new();
        for (i, handle) in handles.into_iter().enumerate() {
            match handle.await.unwrap() {
                Ok(c) => created.push((i % 2, c)),
                Err(CallError::ConcurrentCallLimitExceeded { limit: 2, .. }) => {}
                Err(other) => panic!("round {round}: unexpected {other:?}"),
            }
        }
        assert_eq!(created.len(), 2, "round {round}: the tenant ceiling");
        let active = format!(
            "SELECT count(*) FROM calls WHERE tenant_id = '{}' AND current_state = 'REQUESTED'",
            w.consumer.tenant.id
        );
        assert_eq!(scalar_i64(w.h.conn(), &active).await, 2);
        // Terminal Calls stop counting: the next round starts from zero.
        for (app, c) in created {
            let ctx = if app == 0 { &w.ctx } else { &*ctx2 };
            w.calls()
                .transition(
                    &CallCommandContext::customer(ctx),
                    c.call.id,
                    c.call.version,
                    CallAction::Cancel,
                )
                .await
                .unwrap();
        }
        assert_eq!(scalar_i64(w.h.conn(), &active).await, 0, "round {round}");
    }
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

async fn notices(w: &CallWorld, event_type: &str, limit: &str) -> Vec<Value> {
    strings(
        w.h.conn(),
        &format!(
            "SELECT body::text FROM webhook_events WHERE event_type = '{event_type}' \
               AND body->'data'->>'limit' = '{limit}' ORDER BY created_at, id"
        ),
    )
    .await
    .into_iter()
    .map(|b| serde_json::from_str(&b).unwrap())
    .collect()
}

#[tokio::test]
async fn limit_notices_are_emitted_once_per_window() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    subscribe_limits(&w).await;

    // Request rate: 5 per minute for one credential.
    let limited = w.h.issue(w.consumer.sa(w.kind), &[names::CALLS_READ]).await;
    w.h.platform
        .set_credential_limits(
            limited.credential.id,
            LimitValues {
                requests_per_minute: Some(5),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let token = limited.secret.expose_once().to_string();
    let mut statuses = Vec::new();
    for _ in 0..8 {
        statuses.push(w.h.whoami(&token).await.status.as_u16());
    }
    assert_eq!(statuses, [200, 200, 200, 200, 200, 429, 429, 429]);
    let warnings = notices(&w, "limit.warning", "REQUEST_RATE").await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        warnings[0]["data"],
        json!({"limit": "REQUEST_RATE", "scope": "CREDENTIAL", "window": "MINUTE",
               "threshold": 80, "current": 4, "ceiling": 5})
    );
    let exceeded = notices(&w, "limit.exceeded", "REQUEST_RATE").await;
    assert_eq!(exceeded.len(), 1, "a burst is one notice: {exceeded:?}");
    assert_eq!(exceeded[0]["data"]["current"], 6);
    for event in warnings.iter().chain(exceeded.iter()) {
        common::contracts::check(
            "platform/webhooks/v1.schema.json#/$defs/WebhookEvent",
            event,
        );
    }
    // The next window: a new notice.
    w.h.clock.advance(Duration::seconds(60));
    for _ in 0..7 {
        w.h.whoami(&token).await;
    }
    assert_eq!(notices(&w, "limit.exceeded", "REQUEST_RATE").await.len(), 2);

    // Concurrent calls: credential ceiling 1.
    w.h.platform
        .set_credential_limits(
            w.token.credential.id,
            LimitValues {
                max_concurrent_communications: Some(1),
                requests_per_minute: Some(100_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    w.create().await;
    assert_eq!(
        notices(&w, "limit.warning", "CONCURRENT_CALLS").await.len(),
        1
    );
    for _ in 0..3 {
        let err = w
            .calls()
            .create_requested_call(&w.ctx, &w.initiator, NewCall::of_kind(CallKind::Voice))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            CallError::ConcurrentCallLimitExceeded {
                level: "credential",
                limit: 1
            }
        ));
    }
    let exceeded = notices(&w, "limit.exceeded", "CONCURRENT_CALLS").await;
    assert_eq!(exceeded.len(), 1, "{exceeded:?}");
    assert_eq!(
        exceeded[0]["data"],
        json!({"limit": "CONCURRENT_CALLS", "scope": "CREDENTIAL", "window": "MINUTE",
               "current": 2, "ceiling": 1})
    );
    // Each notice became exactly one delivery to the one subscription.
    assert_eq!(
        scalar_i64(w.h.conn(), "SELECT count(*) FROM webhook_deliveries").await,
        scalar_i64(w.h.conn(), "SELECT count(*) FROM webhook_events").await
    );
    // Never a principal, endpoint, destination or credential.
    for body in strings(w.h.conn(), "SELECT body::text FROM webhook_events").await {
        let v: Value = serde_json::from_str(&body).unwrap();
        for key in v["data"].as_object().unwrap().keys() {
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
        }
    }
}
