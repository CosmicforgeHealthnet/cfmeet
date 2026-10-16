//! Workstream M: the SMS Gateway foundation — schema and its database guards, content
//! protection, the outbox, service health, authentication boundaries, the provider
//! contract and the served API description.

mod common;

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use cf_service_auth::RuntimeEnvironment;
use common::{Harness, TestDb};
use serde_json::{json, Value};
use sms_gateway::{
    domain::RequestContext,
    infra::{
        outbox::{enqueue, Delivery, EventTransport, NewEvent, OutboxPublisher, PublisherSettings},
        RetryPolicy,
    },
    providers::{contract, deterministic::DeterministicSmsProvider, guard_runtime},
    sms::{
        events::{record, EventScope, MessageEvent},
        protection::{mask, Purpose},
        segments::Encoding,
        store::{self, NewTransport},
        threads, Direction, Status, TransportRow,
    },
};
use sms_gateway_migration::MigratorTrait;
use uuid::Uuid;

const REMOTE: &str = "+15550104242";
const TEXT: &str = "canary content 88412 do not log";
const DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn scope() -> EventScope {
    EventScope {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
    }
}

/// A transport written exactly as the domain writes one (sealed, fingerprinted, masked).
async fn seed(h: &Harness, direction: Direction) -> TransportRow {
    let p = &h.state.protection;
    let scope = scope();
    let id = Uuid::new_v4();
    let number = Uuid::new_v4();
    let fp = p.number_fingerprint(REMOTE);
    let masked = mask(REMOTE);
    let thread = threads::touch(h.conn(), scope, number, &fp, &masked, h.now())
        .await
        .unwrap();
    let inbound = direction == Direction::Inbound;
    let key = format!("key-{}", id.simple());
    store::insert(
        h.conn(),
        NewTransport {
            id,
            scope,
            direction,
            status: if inbound {
                Status::Received
            } else {
                Status::Requested
            },
            number_resource_id: number,
            assignment_id: Some(Uuid::new_v4()),
            routing_revision: Some(1),
            principal_id: None,
            target: None,
            remote_sealed: p.seal(Purpose::Remote, id, REMOTE),
            remote_fingerprint: &fp,
            remote_masked: &masked,
            content_sealed: p.seal(Purpose::Content, id, TEXT),
            content_fingerprint: p.content_fingerprint(TEXT),
            encoding: Encoding::Gsm7,
            characters: TEXT.len(),
            segments: 1,
            thread_id: thread.id,
            service_account_id: (!inbound).then(Uuid::new_v4),
            credential_id: None,
            idempotency: (!inbound).then_some((key.as_str(), DIGEST)),
            inbound_grant_id: inbound.then(Uuid::new_v4),
            provider_id: Some("deterministic"),
            max_attempts: 3,
            expires_at: None,
            received_at: inbound.then(|| h.now()),
            correlation_id: "corr_0123456789abcdef",
        },
        h.now(),
    )
    .await
    .unwrap()
}

async fn set_status(h: &Harness, id: Uuid, status: &str) -> Result<(), String> {
    let extra = match status {
        "FAILED" => ", failed_at = now(), failure_code = 'TEST_FAILURE'".to_string(),
        // Authorization stores the released sender (sealed) once.
        "AUTHORIZED" => format!(
            ", sender_sealed = COALESCE(sender_sealed, '{}')",
            h.state.protection.seal(Purpose::Sender, id, "+15550100001")
        ),
        _ => String::new(),
    };
    h.sql(&format!(
        "UPDATE sms_transports SET status = '{status}', version = version + 1 {extra} WHERE id = '{id}'"
    ))
    .await
}

#[tokio::test]
async fn migrations_apply_roll_back_and_reapply_on_an_empty_database() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let count = |db: &TestDb| {
        let conn = db.conn.clone();
        async move {
            use sea_orm::ConnectionTrait;
            conn.query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_name LIKE 'sms\\_%' AND table_name NOT LIKE '%seaql%'",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index::<i64>(0)
            .unwrap()
        }
    };
    sms_gateway_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(count(&db).await, 10, "ten sms_ tables");
    assert!(
        sms_gateway_migration::Migrator::get_pending_migrations(&db.conn)
            .await
            .unwrap()
            .is_empty()
    );
    sms_gateway_migration::Migrator::down(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(count(&db).await, 0, "down removes every table");
    sms_gateway_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(count(&db).await, 10, "re-applies cleanly");
}

#[tokio::test]
async fn the_database_enforces_the_transport_state_machine_and_immutable_facts() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let t = seed(&h, Direction::Outbound).await;
    // Not allowed: REQUESTED -> SUBMITTED / DELIVERED (skipping authorization).
    for bad in ["SUBMITTED", "DELIVERED", "UNKNOWN", "RECEIVED"] {
        let e = set_status(&h, t.id, bad).await.unwrap_err();
        assert!(
            e.contains("not allowed") || e.contains("ck_sms_transports"),
            "{bad}: {e}"
        );
    }
    for ok in ["AUTHORIZED", "SUBMITTING", "UNKNOWN"] {
        set_status(&h, t.id, ok).await.unwrap();
    }
    // UNKNOWN never goes back to a sendable state (no blind resubmission).
    for bad in ["SUBMITTING", "AUTHORIZED", "REQUESTED"] {
        assert!(set_status(&h, t.id, bad).await.is_err(), "UNKNOWN -> {bad}");
    }
    set_status(&h, t.id, "DELIVERED").await.unwrap();
    // Final states stay final.
    assert!(set_status(&h, t.id, "FAILED").await.is_err());
    // The version advances by exactly one.
    let e = h
        .sql(&format!(
            "UPDATE sms_transports SET failure_code = 'X_Y', version = version + 2 WHERE id = '{}'",
            t.id
        ))
        .await
        .unwrap_err();
    assert!(e.contains("version"), "{e}");
    // Facts never change; content is never rewritten; rows are never deleted.
    for change in [
        "tenant_id = gen_random_uuid()",
        "number_resource_id = gen_random_uuid()",
        "remote_fingerprint = repeat('a', 64)",
        "direction = 'INBOUND'",
    ] {
        let e = h
            .sql(&format!(
                "UPDATE sms_transports SET {change}, version = version + 1 WHERE id = '{}'",
                t.id
            ))
            .await
            .unwrap_err();
        assert!(
            e.contains("immutable") || e.contains("ck_sms"),
            "{change}: {e}"
        );
    }
    let other = h.state.protection.seal(Purpose::Content, t.id, "rewritten");
    let e = h
        .sql(&format!(
            "UPDATE sms_transports SET content_sealed = '{other}', version = version + 1 WHERE id = '{}'",
            t.id
        ))
        .await
        .unwrap_err();
    assert!(e.contains("never rewritten"), "{e}");
    let e = h
        .sql(&format!("DELETE FROM sms_transports WHERE id = '{}'", t.id))
        .await
        .unwrap_err();
    assert!(e.contains("never deleted"), "{e}");

    // An inbound transport is RECEIVED and stays so.
    let r = seed(&h, Direction::Inbound).await;
    assert_eq!(r.status(), Status::Received);
    assert!(set_status(&h, r.id, "DELIVERED").await.is_err());
    // An outbound transport cannot be created in another state.
    let e = h
        .sql(&format!(
            "INSERT INTO sms_transports SELECT (x).* FROM (SELECT jsonb_populate_record(NULL::sms_transports, \
               to_jsonb(t) || '{{\"id\": \"{}\", \"status\": \"SUBMITTED\"}}') AS x FROM sms_transports t WHERE id = '{}') s",
            Uuid::new_v4(),
            t.id
        ))
        .await
        .unwrap_err();
    assert!(
        e.contains("starts REQUESTED") || e.contains("ck_sms"),
        "{e}"
    );
}

#[tokio::test]
async fn plaintext_numbers_and_content_are_refused_by_the_schema() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let t = seed(&h, Direction::Outbound).await;
    for (column, value) in [
        ("remote_sealed", REMOTE.to_string()),
        ("remote_masked", REMOTE.to_string()),
        ("remote_fingerprint", REMOTE.to_string()),
    ] {
        let e = h
            .sql(&format!(
                "INSERT INTO sms_transports SELECT (x).* FROM (SELECT jsonb_populate_record(NULL::sms_transports, \
                   to_jsonb(t) || jsonb_build_object('id', gen_random_uuid(), '{column}', '{value}')) AS x \
                   FROM sms_transports t WHERE id = '{}') s",
                t.id
            ))
            .await
            .unwrap_err();
        assert!(e.contains("ck_sms_transports"), "{column}: {e}");
    }
    let e = h
        .sql(&format!(
            "UPDATE sms_transports SET sender_sealed = '{REMOTE}', version = version + 1 WHERE id = '{}'",
            t.id
        ))
        .await
        .unwrap_err();
    assert!(e.contains("ck_sms_transports_sender_sealed"), "{e}");
    // Nothing in the database holds the number or the content in clear.
    let dump = h.dump().await;
    assert!(
        !dump.contains(REMOTE) && !dump.contains("15550104242"),
        "number in clear"
    );
    assert!(!dump.contains("canary content"), "content in clear");
    assert!(dump.contains("+***4242"), "the masked form is stored");
    // The sealed values open only for their own record and purpose.
    let p = &h.state.protection;
    assert_eq!(
        p.open(Purpose::Content, t.id, t.content_sealed.as_deref().unwrap())
            .unwrap(),
        TEXT
    );
    assert!(p
        .open(
            Purpose::Content,
            Uuid::new_v4(),
            t.content_sealed.as_deref().unwrap()
        )
        .is_err());
    assert!(p
        .open(Purpose::Remote, t.id, t.content_sealed.as_deref().unwrap())
        .is_err());
    assert_eq!(
        p.open(Purpose::Remote, t.id, &t.remote_sealed).unwrap(),
        REMOTE
    );
}

#[tokio::test]
async fn at_most_one_live_attempt_and_none_after_an_unknown_outcome() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let t = seed(&h, Direction::Outbound).await;
    let attempt = |n: i32, status: &str| {
        let lease = if status == "CLAIMED" {
            "now() + interval '1 minute'"
        } else {
            "NULL"
        };
        let finished = if status == "CLAIMED" { "NULL" } else { "now()" };
        let owner = if status == "CLAIMED" { "'w1'" } else { "NULL" };
        format!(
            "INSERT INTO sms_attempts (id, transport_id, attempt_number, status, provider_id, \
               lease_owner, lease_until, started_at, finished_at) \
             VALUES (gen_random_uuid(), '{}', {n}, '{status}', 'deterministic', {owner}, {lease}, now(), {finished})",
            t.id
        )
    };
    h.sql(&attempt(1, "CLAIMED")).await.unwrap();
    let e = h.sql(&attempt(2, "CLAIMED")).await.unwrap_err();
    assert!(e.contains("uq_sms_attempts_live"), "{e}");
    h.sql(&format!(
        "UPDATE sms_attempts SET status = 'UNKNOWN', lease_owner = NULL, lease_until = NULL, finished_at = now() WHERE transport_id = '{}'",
        t.id
    ))
    .await
    .unwrap();
    // After an unknown outcome, no further attempt can exist.
    let e = h.sql(&attempt(2, "CLAIMED")).await.unwrap_err();
    assert!(e.contains("uq_sms_attempts_live"), "{e}");
    // And the UNKNOWN attempt cannot be quietly marked unsent.
    let e = h
        .sql(&format!(
            "UPDATE sms_attempts SET status = 'NOT_SENT' WHERE transport_id = '{}'",
            t.id
        ))
        .await
        .unwrap_err();
    assert!(e.contains("provider evidence"), "{e}");
    let e = h
        .sql(&format!(
            "DELETE FROM sms_attempts WHERE transport_id = '{}'",
            t.id
        ))
        .await
        .unwrap_err();
    assert!(e.contains("never deleted"), "{e}");
}

#[tokio::test]
async fn the_outbox_refuses_payloads_with_content_numbers_or_grants() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    for payload in [
        json!({ "content": "hello" }),
        json!({ "body": "hello" }),
        json!({ "e164": "x" }),
        json!({ "sender_grant": "x" }),
        json!({ "note": "call +15550104242" }),
    ] {
        let e = enqueue(
            h.conn(),
            NewEvent {
                event_type: "sms-gateway.message.requested",
                aggregate_type: "SMS_MESSAGE",
                aggregate_id: Uuid::new_v4(),
                scope: None,
                payload: payload.clone(),
            },
            &ctx,
            h.now(),
        )
        .await
        .unwrap_err();
        assert!(
            e.to_string().contains("ck_sms_outbox_events_payload"),
            "{payload}: {e}"
        );
    }
    // The real message event of a transport is accepted and carries no secret.
    let t = seed(&h, Direction::Outbound).await;
    record(h.conn(), &t, MessageEvent::Requested, &ctx, h.now())
        .await
        .unwrap();
    let payload = h
        .text("SELECT payload::text FROM sms_outbox_events WHERE event_type = 'sms-gateway.message.requested'")
        .await
        .unwrap();
    assert!(
        payload.contains("+***4242") && !payload.contains(REMOTE),
        "{payload}"
    );
}

struct ScriptedTransport {
    outcomes: Mutex<Vec<Delivery>>,
    seen: Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl EventTransport for ScriptedTransport {
    async fn deliver(&self, envelope: &Value) -> Delivery {
        self.seen.lock().unwrap().push(envelope.clone());
        let mut o = self.outcomes.lock().unwrap();
        if o.is_empty() {
            Delivery::Delivered
        } else {
            o.remove(0)
        }
    }
}

#[tokio::test]
async fn the_outbox_publishes_in_order_retries_and_dead_letters() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    let t = seed(&h, Direction::Outbound).await;
    for e in [MessageEvent::Requested, MessageEvent::Submitted] {
        record(h.conn(), &t, e, &ctx, h.now()).await.unwrap();
    }
    let transport = Arc::new(ScriptedTransport {
        outcomes: Mutex::new(vec![Delivery::Retryable("timeout".into())]),
        seen: Mutex::new(Vec::new()),
    });
    let publisher = OutboxPublisher::new(
        h.conn().clone(),
        transport.clone(),
        Arc::new(h.clock.clone()),
        PublisherSettings {
            lease: chrono::Duration::seconds(30),
            max_attempts: 2,
            retry: RetryPolicy::default(),
            batch: 10,
        },
        "test",
    );
    // First pass: the first event fails transiently; the second waits behind it (order).
    let r = publisher.publish_due().await.unwrap();
    assert_eq!((r.claimed, r.retried, r.published), (1, 1, 0));
    h.advance(3600);
    let r = publisher.publish_due().await.unwrap();
    assert_eq!(r.published, 1, "{r:?}");
    let r = publisher.publish_due().await.unwrap();
    assert_eq!(r.published, 1, "{r:?}");
    let seen = transport.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[2]["event_type"], "sms-gateway.message.submitted");
    assert_eq!(seen[0]["producer_service"], "cfmeet.sms-gateway");
    assert!(!seen.iter().any(|e| e.to_string().contains(REMOTE)));
    common::conforms("platform/events/envelope/v1.schema.json", &seen[0]);

    // A permanently refusing receiver dead-letters, and readiness reports it.
    record(h.conn(), &t, MessageEvent::Delivered, &ctx, h.now())
        .await
        .unwrap();
    transport
        .outcomes
        .lock()
        .unwrap()
        .push(Delivery::Rejected("contract".into()));
    let r = publisher.publish_due().await.unwrap();
    assert_eq!(r.dead_lettered, 1);
    let e = h
        .sql("UPDATE sms_outbox_events SET status = 'PENDING' WHERE status = 'DEAD_LETTER'")
        .await
        .unwrap_err();
    assert!(e.contains("final"), "{e}");
    let ready = h.get(None, "/ready").await;
    assert_eq!(ready.json["outboxDeadLetter"], 1, "{}", ready.json);
}

#[tokio::test]
async fn health_and_readiness_report_counts_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.get(None, "/health").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["service"], "sms-gateway");
    let r = h.get(None, "/ready").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["migrations"], "current");
    assert_eq!(r.json["provider"], "configured");
    common::conforms_component("Readiness", &r.json);

    // Pending migrations are not ready.
    let db = Arc::new(TestDb::empty().await.unwrap());
    let fresh = Harness::on(db, |_| {});
    let r = fresh.get(None, "/ready").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.json["migrations"], "pending");
}

#[tokio::test]
async fn customer_routes_require_a_platform_access_credential_and_fail_closed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["sms.read"]);
    for path in ["/v1/sms/messages", "/v1/sms/anything-unknown"] {
        let r = h.get(None, path).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(r.code(), "AUTHENTICATION_FAILED");
        assert!(r.headers.contains_key("www-authenticate"));
        let r = h.get(Some("cfk_test_not_a_credential"), path).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    // An authenticated unknown route is a uniform 404 envelope.
    let r = h.get(Some(&c.token), "/v1/sms/anything-unknown").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.code(), "NOT_FOUND");
    common::conforms("sms-gateway/common/v1.schema.json#/$defs/Error", &r.json);
    // Platform Access unreachable: refused, never anonymous.
    *h.authority.down.lock().unwrap() = true;
    let r = h.get(Some(&c.token), "/v1/sms/anything-unknown").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    *h.authority.down.lock().unwrap() = false;
    // A service token is never a customer credential.
    let signed = h
        .peers
        .telephony
        .sign(&cf_service_auth::OutboundServiceRequest::new_operation(
            cf_service_auth::ServiceId::sms_gateway(),
        ))
        .unwrap();
    let mut b = axum::http::Request::builder().uri("/v1/sms/anything-unknown");
    for (k, v) in signed.headers().iter() {
        b = b.header(k, v);
    }
    let r = h
        .raw(
            b.header("authorization", format!("Bearer {}", c.token))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn internal_routes_require_a_trusted_service_identity_even_when_unknown() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.get(None, "/internal/v1/anything").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.code(), "INTERNAL_AUTHENTICATION_FAILED");
    // Correctly signed, but the issuer is not in the gateway's trust bundle.
    let r = h
        .internal(&h.peers.telephony, "/internal/v1/anything")
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
    // A customer credential is never a service identity.
    let c = h.customer(&["sms.read"]);
    let r = h.get(Some(&c.token), "/internal/v1/anything").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_served_openapi_is_the_checked_in_contract() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.get(None, "/openapi.json").await;
    assert_eq!(r.status, StatusCode::OK);
    let file: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/contracts/sms-gateway/http/v1.openapi.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(r.json, file, "the served description is the contract");
    // Every documented operation is a registered route (a generic 404 envelope means
    // the router does not serve it).
    let c = h.customer(&["sms.read", "sms.send"]);
    for (path, item) in file["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            let concrete = path
                .split('/')
                .map(|s| match s {
                    // The configured provider (an unknown provider id is a real 404).
                    "{providerId}" => "deterministic".to_string(),
                    s if s.starts_with('{') => Uuid::new_v4().to_string(),
                    s => s.to_string(),
                })
                .collect::<Vec<_>>()
                .join("/");
            let request = axum::http::Request::builder()
                .method(method.to_uppercase().as_str())
                .uri(&concrete)
                .header("authorization", format!("Bearer {}", c.token))
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap();
            let r = h.raw(request).await;
            let unrouted = r.status == StatusCode::METHOD_NOT_ALLOWED
                || (r.status == StatusCode::NOT_FOUND
                    && r.json["error"]["message"] == sms_gateway::http::NO_ROUTE_MESSAGE);
            assert!(
                !unrouted,
                "{method} {path} is documented but not served ({})",
                r.status
            );
        }
    }
}

#[tokio::test]
async fn the_deterministic_provider_passes_the_adapter_contract_and_is_refused_outside_local_and_test(
) {
    let provider = DeterministicSmsProvider::new("deterministic", common::CALLBACK_SECRET);
    contract::run(&provider, chrono::Utc::now()).await.unwrap();
    assert!(guard_runtime(&provider, RuntimeEnvironment::Local).is_ok());
    assert!(guard_runtime(&provider, RuntimeEnvironment::Test).is_ok());
    assert!(guard_runtime(&provider, RuntimeEnvironment::Staging).is_err());
    assert!(guard_runtime(&provider, RuntimeEnvironment::Production).is_err());
}

#[tokio::test]
async fn concurrent_publishers_deliver_each_event_once_and_a_dead_publishers_claim_is_taken_over() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    // Ten messages, two events each (per-message order).
    for _ in 0..10 {
        let t = seed(&h, Direction::Outbound).await;
        record(h.conn(), &t, MessageEvent::Requested, &ctx, h.now())
            .await
            .unwrap();
        record(h.conn(), &t, MessageEvent::Blocked, &ctx, h.now())
            .await
            .unwrap();
    }
    let transport = Arc::new(ScriptedTransport {
        outcomes: Mutex::new(Vec::new()),
        seen: Mutex::new(Vec::new()),
    });
    let settings = PublisherSettings {
        lease: chrono::Duration::seconds(30),
        max_attempts: 5,
        retry: RetryPolicy::default(),
        batch: 4,
    };
    let publisher = |db: sea_orm::DatabaseConnection| {
        Arc::new(OutboxPublisher::new(
            db,
            transport.clone(),
            Arc::new(h.clock.clone()),
            settings,
            "test",
        ))
    };
    // A publisher claims and dies before delivering: its claim expires, then is taken over.
    let dead = publisher(h.conn().clone());
    let dead_token = Uuid::new_v4();
    let claimed = dead.claim(dead_token).await.unwrap();
    assert_eq!(claimed.len(), 4);
    let (a, b) = (publisher(h.conn().clone()), publisher(h.conn().clone()));
    let before = a.publish_due().await.unwrap();
    assert!(
        before.claimed <= 6,
        "a live claim is not taken over: {before:?}"
    );
    h.advance(60);
    for _ in 0..10 {
        let (ra, rb) = tokio::join!(a.publish_due(), b.publish_due());
        let (ra, rb) = (ra.unwrap(), rb.unwrap());
        assert_eq!(ra.lost_claims + rb.lost_claims, 0);
    }
    let seen = transport.seen.lock().unwrap().clone();
    let mut ids: Vec<String> = seen
        .iter()
        .map(|e| e["event_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 20, "every event delivered");
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 20, "no event delivered twice");
    // Per message, requested precedes blocked.
    let order: Vec<(String, String)> = seen
        .iter()
        .map(|e| {
            (
                e["payload"]["message_id"].as_str().unwrap().to_string(),
                e["event_type"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for (i, (m, t)) in order.iter().enumerate() {
        if t.ends_with(".blocked") {
            assert!(order[..i]
                .iter()
                .any(|(m2, t2)| m2 == m && t2.ends_with(".requested")));
        }
    }
    // The dead publisher's late completion is refused (its claim was taken over).
    let mut report = Default::default();
    dead.deliver_claimed(&claimed[0], dead_token, &mut report)
        .await
        .unwrap();
    assert_eq!(report.lost_claims, 1);
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_outbox_events WHERE status = 'PUBLISHED'")
            .await,
        20
    );
}

#[tokio::test]
async fn rate_windows_count_hits_and_the_sweep_purges_only_expired_windows() {
    let Some(h) = Harness::new().await else {
        return;
    };
    use sms_gateway::sms::{controls, workers};
    let minute = chrono::Duration::minutes(1);
    for i in 1..=3 {
        let within = controls::hit(h.conn(), "ENVIRONMENT", "env-a", minute, 2, h.now())
            .await
            .unwrap();
        assert_eq!(within, i <= 2, "hit {i}");
    }
    // A new window starts from zero.
    h.advance(60);
    assert!(
        controls::hit(h.conn(), "ENVIRONMENT", "env-a", minute, 2, h.now())
            .await
            .unwrap()
    );
    // Two concurrent passes purge each expired window once; current windows stay.
    h.advance(3 * 86_400);
    controls::hit(h.conn(), "ENVIRONMENT", "env-a", minute, 2, h.now())
        .await
        .unwrap();
    let (a, b) = tokio::join!(workers::sweep(&h.state), workers::sweep(&h.state));
    assert_eq!(
        a.unwrap().rate_windows_purged + b.unwrap().rate_windows_purged,
        2
    );
    assert_eq!(h.scalar("SELECT count(*) FROM sms_rate_windows").await, 1);
}
