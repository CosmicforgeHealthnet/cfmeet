//! Part V H §103 — communication-context authority reaches Messaging.
//!
//! The database enqueues exactly one job per context version of a MESSAGING-enabled
//! context (at commit, whatever code path changed it); the worker delivers them in
//! version order over signed internal HTTP with the owner's delegated scope, retries on
//! failure, and never sends a contact value.

mod common;

use std::sync::{Arc, Mutex};

use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind as Kind, InternalPermission, RuntimeEnvironment, ServiceId,
    ServiceTokenVerifier, TrustPolicy,
};
use common::*;
use privacy_relay::{messaging_authority::HttpMessagingAuthority, service::RelayService};
use serde_json::{json, Value};

#[derive(Clone)]
struct MockMessaging {
    verifier: Arc<ServiceTokenVerifier>,
    received: Arc<Mutex<Vec<(Value, Value)>>>,
    /// Answer with 503 while set.
    down: Arc<Mutex<bool>>,
}

async fn mock_apply(
    State(m): State<MockMessaging>,
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
        .require_permission(InternalPermission::MessagingContextAuthorityApply)
        .is_err()
    {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({ "error": { "code": "INTERNAL_PERMISSION_DENIED" } })),
        );
    }
    if *m.down.lock().unwrap() {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": { "code": "SERVICE_UNAVAILABLE" } })),
        );
    }
    let delegated = serde_json::to_value(ctx.delegated_platform_context().unwrap()).unwrap();
    let mut received = m.received.lock().unwrap();
    // A replay of an already-applied version answers STALE (Messaging's fence).
    let stale = received.iter().any(|(_, b)| {
        b["reference"] == body["reference"]
            && b["sourceVersion"].as_i64() >= body["sourceVersion"].as_i64()
    });
    received.push((delegated, body.clone()));
    let answer = if stale {
        json!({ "applied": false, "reason": "STALE_VERSION" })
    } else {
        json!({ "applied": true, "state": body["state"], "conversationsAffected": 1 })
    };
    (axum::http::StatusCode::OK, Json(answer))
}

async fn messaging_relay(h: &Harness) -> (RelayService, MockMessaging) {
    let env = RuntimeEnvironment::Test;
    // Messaging's bundle grants Privacy Relay messaging-context-authority.apply only.
    let policy = TrustPolicy::from_json(
        &trust_bundle_json(
            cf_service_auth::services::MESSAGING,
            env,
            &[(
                cf_service_auth::services::PRIVACY_RELAY,
                vec![h.ids.relay_jwk.clone()],
                &["messaging-context-authority.apply"],
            )],
        ),
        &ServiceId::messaging(),
        env,
    )
    .unwrap();
    let mock = MockMessaging {
        verifier: Arc::new(ServiceTokenVerifier::new(
            policy,
            Arc::new(cf_service_auth::SystemClock),
        )),
        received: Arc::default(),
        down: Arc::default(),
    };
    let app = Router::new()
        .route("/internal/v1/context-authorities", post(mock_apply))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut targets = InternalTargets::new(env);
    targets.insert(ServiceId::messaging(), &base).unwrap();
    let client = Arc::new(
        InternalServiceClient::new(
            h.ids.relay.clone(),
            targets,
            std::time::Duration::from_secs(5),
        )
        .unwrap(),
    );
    let relay = h
        .sibling()
        .with_messaging_authority(Arc::new(HttpMessagingAuthority::new(client)));
    (relay, mock)
}

fn messaging_context(subjects: &[uuid::Uuid]) -> Value {
    let mut req = context_request(subjects, "BIDIRECTIONAL", &[]);
    req["allowedChannels"] = json!(["VOICE", "MESSAGING"]);
    req
}

#[tokio::test]
async fn every_context_version_reaches_messaging_once_in_order_without_contact_data() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (relay, mock) = messaging_relay(&h).await;
    let c = Consumer::new(Kind::Sandbox);
    let [a, b] = principals(2).try_into().unwrap();
    // A voice-only context never concerns Messaging.
    h.bidirectional(&c, &[a, b]).await;
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_messaging_authority_jobs")
            .await,
        0
    );
    let ctx = h.create_context(&c, messaging_context(&[a, b])).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    h.lifecycle(&c, &id, "suspend", 1).await;
    h.lifecycle(&c, &id, "resume", 2).await;
    h.lifecycle(&c, &id, "revoke", 3).await;
    // One durable job per version, enqueued by the database at commit.
    let states = rows(
        h.conn(),
        &format!("SELECT context_version::text, authority_state, status FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{id}' ORDER BY context_version"),
    )
    .await;
    assert_eq!(
        states,
        vec![
            vec!["1".to_string(), "ACTIVE".into(), "PENDING".into()],
            vec!["2".into(), "SUSPENDED".into(), "PENDING".into()],
            vec!["3".into(), "ACTIVE".into(), "PENDING".into()],
            vec!["4".into(), "REVOKED".into(), "PENDING".into()],
        ]
    );
    assert_eq!(relay.run_messaging_authority_sweep(100).await.unwrap(), 4);
    let received = mock.received.lock().unwrap().clone();
    assert_eq!(received.len(), 4);
    let versions: Vec<i64> = received
        .iter()
        .map(|(_, b)| b["sourceVersion"].as_i64().unwrap())
        .collect();
    assert_eq!(versions, vec![1, 2, 3, 4], "version order per context");
    let (delegated, body) = &received[3];
    assert_eq!(delegated["tenant_id"], json!(c.tenant_id));
    assert_eq!(
        delegated["application_environment_id"],
        json!(c.environment_id)
    );
    assert_eq!(body["namespace"], "privacy-relay");
    assert_eq!(body["reference"], json!(id));
    assert_eq!(body["state"], "REVOKED");
    let mut keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["namespace", "reason", "reference", "sourceVersion", "state"]
    );
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{id}' AND status = 'APPLIED'")).await,
        4
    );
    // Applied jobs are final.
    assert!(exec(h.conn(), &format!("UPDATE privacy_relay_messaging_authority_jobs SET status = 'PENDING' WHERE context_id = '{id}'")).await.is_err());
    assert!(exec(
        h.conn(),
        "DELETE FROM privacy_relay_messaging_authority_jobs"
    )
    .await
    .is_err());
}

#[tokio::test]
async fn messaging_outages_are_retried_and_stale_answers_count_as_applied() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (relay, mock) = messaging_relay(&h).await;
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.create_context(&c, messaging_context(&[a, b])).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    *mock.down.lock().unwrap() = true;
    h.lifecycle(&c, &id, "revoke", 1).await;
    // The revocation is committed locally even though Messaging is down.
    assert_eq!(relay.run_messaging_authority_sweep(100).await.unwrap(), 0);
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{id}' AND status = 'PENDING' AND attempt_count = 1 AND last_error_category = 'UNAVAILABLE' AND next_attempt_at > now()")).await,
        2,
        "both versions wait with backoff"
    );
    *mock.down.lock().unwrap() = false;
    exec(h.conn(), "UPDATE privacy_relay_messaging_authority_jobs SET next_attempt_at = now() - interval '1 second' WHERE status = 'PENDING'").await.unwrap();
    assert_eq!(relay.run_messaging_authority_sweep(100).await.unwrap(), 2);
    // A replayed (already applied) version is acknowledged as STALE and still completes.
    exec(h.conn(), &format!(
        "INSERT INTO privacy_relay_messaging_authority_jobs (id, tenant_id, application_id, environment_id, environment_kind, \
           on_behalf_of_service_account_id, context_id, context_version, authority_state, reason) \
         SELECT gen_random_uuid(), tenant_id, application_id, environment_id, environment_kind, created_by_service_account_id, \
           id, 0, 'ACTIVE', 'CONTEXT_ACTIVE' FROM communication_contexts WHERE id = '{id}'"
    ))
    .await
    .unwrap();
    assert_eq!(relay.run_messaging_authority_sweep(100).await.unwrap(), 1);
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE result_snapshot->>'reason' = 'STALE_VERSION'").await,
        1
    );
    // Unconfigured Messaging: jobs stay durably PENDING (never falsely applied).
    let ctx = h.create_context(&c, messaging_context(&[a, b])).await;
    let other = ctx["contextId"].as_str().unwrap().to_string();
    h.sibling()
        .run_messaging_authority_sweep(100)
        .await
        .unwrap();
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{other}' AND status = 'PENDING' AND last_error_category = 'NOT_CONFIGURED'")).await,
        1
    );
}
