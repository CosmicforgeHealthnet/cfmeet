//! §29: durable, synchronous-first active-call enforcement.

mod common;

use std::sync::{Arc, Mutex};

use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind as Kind, InternalPermission, RuntimeEnvironment, ServiceId,
    ServiceTokenVerifier, TrustPolicy,
};
use chrono::Duration;
use common::*;
use privacy_relay::{
    enforcement::{DeliveryFailure, HttpCallingEnforcer},
    service::RelayService,
};
use serde_json::{json, Value};

#[tokio::test]
async fn revocation_commits_even_when_calling_is_down_and_is_retried_until_applied() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let sid = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();

    h.calling.fail_with(Some(DeliveryFailure::Unavailable));
    let key = "revoke-while-calling-down";
    let r = h
        .keyed(
            &format!("/internal/v1/contexts/{id}/revoke"),
            &c,
            key,
            json!({ "expectedVersion": 1 }),
        )
        .await;
    assert_eq!(r.json["context"]["status"], "REVOKED");
    // The response does not claim enforcement it could not confirm.
    assert_eq!(r.json["enforcement"]["status"], "PENDING");
    let event_id = r.json["enforcement"]["enforcementEventIds"][0]
        .as_str()
        .unwrap()
        .to_string();
    // Authority is revoked locally regardless.
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "CONTEXT_REVOKED");
    assert_eq!(h.count("SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE status = 'PENDING' AND attempt_count = 1 AND last_error_category = 'UNAVAILABLE'").await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM relay_audit_events WHERE event_type = 'ENFORCEMENT_FAILED'")
            .await,
        1
    );

    // Still down at the next due attempt: backoff, still pending, no duplicate failure event.
    h.clock.advance(Duration::seconds(5));
    assert_eq!(h.relay.run_enforcement_sweep(10).await.unwrap(), 0);
    assert_eq!(
        h.count("SELECT attempt_count::bigint FROM privacy_relay_enforcement_jobs")
            .await,
        2
    );
    assert_eq!(
        h.count("SELECT count(*) FROM relay_audit_events WHERE event_type = 'ENFORCEMENT_FAILED'")
            .await,
        1
    );
    // Not due yet: nothing is attempted.
    assert_eq!(h.relay.run_enforcement_sweep(10).await.unwrap(), 0);
    assert_eq!(
        h.count("SELECT attempt_count::bigint FROM privacy_relay_enforcement_jobs")
            .await,
        2
    );

    // Calling recovers; the worker applies the job.
    h.calling.fail_with(None);
    h.clock.advance(Duration::minutes(10));
    assert_eq!(h.relay.run_enforcement_sweep(10).await.unwrap(), 1);
    assert_eq!(h.calling.delivered_event_ids(), vec![event_id.clone()]);
    assert_eq!(h.count("SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE status = 'APPLIED' AND completed_at IS NOT NULL").await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM relay_audit_events WHERE event_type = 'ENFORCEMENT_APPLIED'")
            .await,
        1
    );
    // A replay of the revoke now reports the applied enforcement.
    let replay = h
        .keyed(
            &format!("/internal/v1/contexts/{id}/revoke"),
            &c,
            key,
            json!({ "expectedVersion": 1 }),
        )
        .await;
    assert_eq!(replay.json["enforcement"]["status"], "APPLIED");
    // Nothing is delivered twice.
    assert_eq!(h.relay.run_enforcement_sweep(10).await.unwrap(), 0);
    assert_eq!(
        h.relay
            .deliver_enforcement(event_id.parse().unwrap())
            .await
            .unwrap()
            .as_str(),
        "APPLIED"
    );
    assert_eq!(h.calling.delivered().len(), 1);
}

#[tokio::test]
async fn concurrent_retries_deliver_each_job_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    h.calling.fail_with(Some(DeliveryFailure::Timeout));
    let mut ids = Vec::new();
    for _ in 0..6 {
        let ctx = h.bidirectional(&c, &[a, b]).await;
        let id = ctx["contextId"].as_str().unwrap().to_string();
        h.lifecycle(&c, &id, "revoke", 1).await;
        ids.push(id);
    }
    h.calling.fail_with(None);
    h.clock.advance(Duration::minutes(5));
    let instances: Vec<RelayService> = (0..4).map(|_| h.sibling()).collect();
    let applied =
        futures::future::join_all(instances.iter().map(|r| r.run_enforcement_sweep(50))).await;
    let total: usize = applied.into_iter().map(|r| r.unwrap()).sum();
    assert_eq!(total, 6);
    let mut delivered = h.calling.delivered_event_ids();
    delivered.sort();
    delivered.dedup();
    assert_eq!(delivered.len(), 6);
    assert_eq!(
        h.calling.delivered().len(),
        6,
        "each job delivered exactly once"
    );
}

#[derive(Clone)]
struct MockCalling {
    verifier: Arc<ServiceTokenVerifier>,
    received: Arc<Mutex<Vec<(Value, Value)>>>,
}

async fn mock_enforce(
    State(m): State<MockCalling>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    let Ok(ctx) = m.verifier.verify_headers(&headers) else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "code": "INTERNAL_AUTHENTICATION_FAILED" } })),
        );
    };
    if ctx
        .require_permission(InternalPermission::CommunicationAuthorityEnforce)
        .is_err()
    {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({ "error": { "code": "INTERNAL_PERMISSION_DENIED" } })),
        );
    }
    let delegated = serde_json::to_value(ctx.delegated_platform_context().unwrap()).unwrap();
    m.received.lock().unwrap().push((delegated, body.clone()));
    (
        axum::http::StatusCode::OK,
        Json(
            json!({ "status": "APPLIED", "enforcementEventId": body["enforcementEventId"], "matchedCalls": 1, "terminatedCalls": 1 }),
        ),
    )
}

#[tokio::test]
async fn http_enforcer_uses_service_auth_and_the_delegated_owner_scope() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let env = RuntimeEnvironment::Test;
    // Calling's trust bundle grants Privacy Relay communication-authority.enforce.
    let policy = TrustPolicy::from_json(
        &trust_bundle_json(
            cf_service_auth::services::CALLING,
            env,
            &[(
                cf_service_auth::services::PRIVACY_RELAY,
                vec![h.ids.relay_jwk.clone()],
                &["communication-authority.enforce"],
            )],
        ),
        &ServiceId::calling(),
        env,
    )
    .unwrap();
    let mock = MockCalling {
        verifier: Arc::new(ServiceTokenVerifier::new(
            policy,
            Arc::new(cf_service_auth::SystemClock),
        )),
        received: Arc::default(),
    };
    let app = Router::new()
        .route("/internal/v1/privacy-relay/enforcement", post(mock_enforce))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut targets = InternalTargets::new(env);
    targets.insert(ServiceId::calling(), &base).unwrap();
    let client = Arc::new(
        InternalServiceClient::new(
            h.ids.relay.clone(),
            targets,
            std::time::Duration::from_secs(5),
        )
        .unwrap(),
    );
    let relay = RelayService::new(
        h.conn().clone(),
        Arc::new(h.clock.clone()),
        h.feature.clone(),
        env,
        Arc::new(HttpCallingEnforcer::new(client)),
    );

    let c = Consumer::new(Kind::Sandbox);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id: uuid::Uuid = ctx["contextId"].as_str().unwrap().parse().unwrap();
    // Revoke through the service wired to the HTTP enforcer.
    let cmd =
        privacy_relay::scope::CommandContext::for_delegated(&c.delegated(), ServiceId::calling());
    let out = relay
        .revoke_context(
            &cmd,
            privacy_relay::commands::IdempotencyKey::parse("http-revoke").unwrap(),
            id,
            serde_json::from_value(json!({ "expectedVersion": 1 })).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(out.body["enforcement"]["status"], "APPLIED");
    let received = mock.received.lock().unwrap().clone();
    assert_eq!(received.len(), 1);
    let (delegated, body) = &received[0];
    assert_eq!(delegated["tenant_id"], json!(c.tenant_id));
    assert_eq!(delegated["application_id"], json!(c.application_id));
    assert_eq!(
        delegated["application_environment_id"],
        json!(c.environment_id)
    );
    assert_eq!(delegated["application_environment_kind"], "SANDBOX");
    assert_eq!(body["contextId"], json!(id));
    assert_eq!(body["reason"], "CONTEXT_REVOKED");
    // Only safe identifiers cross the boundary.
    let mut keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "affectedPrincipalIds",
            "contextId",
            "contextVersion",
            "enforcementEventId",
            "reason",
            "relaySessionIds",
            "terminateActiveCalls"
        ]
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE status = 'APPLIED'")
            .await,
        1
    );

    // Unconfigured Calling: the job stays durably pending (never falsely applied).
    let unconfigured = RelayService::new(
        h.conn().clone(),
        Arc::new(h.clock.clone()),
        h.feature.clone(),
        env,
        Arc::new(HttpCallingEnforcer::new(Arc::new(
            InternalServiceClient::new(
                h.ids.relay.clone(),
                InternalTargets::new(env),
                std::time::Duration::from_secs(1),
            )
            .unwrap(),
        ))),
    );
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id: uuid::Uuid = ctx["contextId"].as_str().unwrap().parse().unwrap();
    let out = unconfigured
        .revoke_context(
            &cmd,
            privacy_relay::commands::IdempotencyKey::parse("unconfigured").unwrap(),
            id,
            serde_json::from_value(json!({ "expectedVersion": 1 })).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(out.body["enforcement"]["status"], "PENDING");
    assert_eq!(h.count("SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE last_error_category = 'NOT_CONFIGURED'").await, 1);
}
