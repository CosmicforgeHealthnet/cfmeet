//! Part V D.13 / D.14: the Conversation aggregate — context classes, persisted security
//! profiles and their policy, no silent downgrade, lifecycle and concurrency, isolation,
//! idempotency, context references, expiry, outbox consistency and sequencing.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use messaging::identity::PrincipalStatus;
use serde_json::{json, Value};
use uuid::Uuid;

const ALL: &[&str] = &[
    "messages.send",
    "messages.read",
    "conversations.create",
    "conversations.manage",
];

impl Harness {
    async fn create(&self, c: &Customer, acting: Uuid, body: Value, key: &str) -> Response {
        self.send(
            Method::POST,
            Some(&c.token),
            Some(acting),
            "/v1/conversations",
            Some(body),
            &[("idempotency-key", key)],
        )
        .await
    }

    async fn patch_conv(
        &self,
        c: &Customer,
        acting: Option<Uuid>,
        id: &Value,
        body: Value,
    ) -> Response {
        self.send(
            Method::PATCH,
            Some(&c.token),
            acting,
            &format!("/v1/conversations/{}", id.as_str().unwrap()),
            Some(body),
            &[],
        )
        .await
    }

    async fn put_policy(&self, c: &Customer, expected: i64, policy: Value) -> Response {
        self.send(
            Method::PUT,
            Some(&c.token),
            None,
            "/v1/messaging/policy",
            Some(json!({ "expectedVersion": expected, "policy": policy })),
            &[],
        )
        .await
    }

    /// Two principals of `c` with one active device each.
    async fn pair(&self, c: &Customer) -> (Uuid, Uuid) {
        let a = self.principal(c);
        let b = self.principal(c);
        self.active_device(c, a).await;
        self.active_device(c, b).await;
        (a, b)
    }
}

fn key() -> String {
    format!("idem-{}", Uuid::new_v4().simple())
}

#[tokio::test]
async fn a_direct_conversation_defaults_to_private_e2ee_and_is_deduplicated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, bob) = h.pair(&c).await;
    let body = json!({ "contextClass": "DIRECT", "counterpartPrincipalId": bob });
    let r = h.create(&c, alice, body.clone(), &key()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/Conversation",
        &r.json,
    );
    assert_eq!(r.json["securityProfile"], "PRIVATE_E2EE");
    assert_eq!(r.json["encryption"]["protocol"], "MLS_1_0");
    assert_eq!(r.json["encryption"]["serverCanReadContent"], false);
    assert_eq!(unb64(&r.json["encryption"]["mlsGroupId"]).len(), 32);
    assert_eq!(r.json["lifecycleState"], "ACTIVE");
    assert_eq!(r.json["members"].as_array().unwrap().len(), 2);
    // The same pair, from either side, gets the same conversation.
    let again = h
        .create(
            &c,
            bob,
            json!({ "contextClass": "DIRECT", "counterpartPrincipalId": alice }),
            &key(),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.json["conversationId"], r.json["conversationId"]);
    // A conversation with oneself is not a direct conversation.
    let r = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "DIRECT", "counterpartPrincipalId": alice }),
            &key(),
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    // The counterpart must be an active messaging principal of the same environment.
    let other = h.customer(ALL);
    let stranger = h.principal(&other);
    let r = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "DIRECT", "counterpartPrincipalId": stranger }),
            &key(),
        )
        .await;
    assert_eq!(r.reason(), "COUNTERPART_NOT_AVAILABLE");
}

#[tokio::test]
async fn concurrent_direct_creations_produce_one_conversation() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, bob) = h.pair(&c).await;
    let mut tasks = Vec::new();
    for i in 0..8 {
        let (from, to) = if i % 2 == 0 {
            (alice, bob)
        } else {
            (bob, alice)
        };
        let state = h.state.clone();
        let token = c.token.clone();
        tasks.push(tokio::spawn(async move {
            use tower::ServiceExt;
            let req = axum::http::Request::post("/v1/conversations")
                .header("authorization", format!("Bearer {token}"))
                .header("cf-acting-principal", from.to_string())
                .header(
                    "idempotency-key",
                    format!("concurrent-{i}-{}", Uuid::new_v4().simple()),
                )
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({ "contextClass": "DIRECT", "counterpartPrincipalId": to }).to_string(),
                ))
                .unwrap();
            let resp = messaging::http::app(state).oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = http_body_util::BodyExt::collect(resp.into_body())
                .await
                .unwrap()
                .to_bytes();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        }));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut created = 0;
    for t in tasks {
        let (status, body) = t.await.unwrap();
        assert!(
            status == StatusCode::CREATED || status == StatusCode::OK,
            "{status} {body}"
        );
        created += (status == StatusCode::CREATED) as usize;
        ids.insert(body["conversationId"].as_str().unwrap().to_string());
    }
    assert_eq!((ids.len(), created), (1, 1));
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_conversations")
            .await,
        1
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE event_type = 'messaging.conversation.created'").await,
        1
    );
}

#[tokio::test]
async fn encryption_that_cannot_be_established_is_an_error_never_a_managed_fallback() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let alice = h.principal(&c);
    let bob = h.principal(&c);
    let body = json!({ "contextClass": "DIRECT", "counterpartPrincipalId": bob });
    let r = h.create(&c, alice, body.clone(), &key()).await;
    assert_eq!(
        (r.status, r.code(), r.reason()),
        (
            StatusCode::CONFLICT,
            "E2EE_NOT_AVAILABLE",
            "CREATOR_HAS_NO_DEVICE"
        )
    );
    h.active_device(&c, alice).await;
    let r = h.create(&c, alice, body.clone(), &key()).await;
    assert_eq!(r.reason(), "COUNTERPART_HAS_NO_DEVICE");
    // Even with MANAGED allowed, an unavailable E2EE request is not converted.
    h.put_policy(
        &c,
        0,
        json!({ "contexts": { "DIRECT": { "allowedProfiles": ["PRIVATE_E2EE", "MANAGED"] } } }),
    )
    .await;
    let r = h.create(&c, alice, body, &key()).await;
    assert_eq!(r.reason(), "COUNTERPART_HAS_NO_DEVICE");
    // Nothing was written by any refused attempt (the transaction rolled back whole).
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_conversations")
            .await,
        0
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_idempotency_keys")
            .await,
        0
    );
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM messaging_outbox_events WHERE aggregate_type = 'CONVERSATION'"
        )
        .await,
        0
    );
    // Deployments without E2EE devices refuse PRIVATE_E2EE creation explicitly.
    let disabled = Harness::on(h.db.clone(), |s| s.devices.enabled = false);
    disabled
        .authority
        .tokens
        .lock()
        .unwrap()
        .insert(c.token.clone(), c.context.clone());
    disabled.set_principal(
        &c,
        alice,
        PrincipalStatus::Active {
            kind: "NATIVE_CF_USER".into(),
        },
    );
    let r = disabled
        .create(&c, alice, json!({ "contextClass": "GROUP" }), &key())
        .await;
    assert_eq!(r.reason(), "E2EE_DEVICES_DISABLED");
}

#[tokio::test]
async fn managed_requires_explicit_policy_and_profiles_are_never_confused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, _) = h.pair(&c).await;
    let r = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "ENTERPRISE", "securityProfile": "MANAGED" }),
            &key(),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("SECURITY_PROFILE_NOT_ALLOWED", "MANAGED_NOT_ALLOWED")
    );
    let r = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "ENTERPRISE", "securityProfile": "E2EE" }),
            &key(),
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR", "unknown profiles are refused");
    let p = h
        .put_policy(&c, 0, json!({ "contexts": { "ENTERPRISE": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } }))
        .await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.text);
    conforms("messaging/common/v1.schema.json#/$defs/PolicyView", &p.json);
    assert_eq!(
        p.json["effective"]["contexts"]["ENTERPRISE"]["legalHoldEligible"],
        true
    );
    let managed = h
        .create(&c, alice, json!({ "contextClass": "ENTERPRISE" }), &key())
        .await;
    assert_eq!(managed.status, StatusCode::CREATED, "{}", managed.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/Conversation",
        &managed.json,
    );
    assert_eq!(managed.json["securityProfile"], "MANAGED");
    assert_eq!(managed.json["encryption"]["serverCanReadContent"], true);
    assert_eq!(managed.json["encryption"]["mlsGroupId"], Value::Null);
    // The other allowed profile can still be chosen explicitly.
    let e2ee = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "ENTERPRISE", "securityProfile": "PRIVATE_E2EE" }),
            &key(),
        )
        .await;
    assert_eq!(e2ee.json["securityProfile"], "PRIVATE_E2EE");
    // The database refuses an inconsistent profile row (MANAGED with an MLS binding).
    assert!(h
        .sql("INSERT INTO messaging_conversations SELECT gen_random_uuid(), tenant_id, application_id, environment_id, context_class, \
              NULL, NULL, NULL, 'MANAGED', 'CFMEET_MANAGED_V1', NULL, mls_group_id, 1, 'ACTIVE', NULL, policy_snapshot_id, 0, 1, \
              created_by_principal_id, created_by_credential_id, owner_principal_id, NULL, now(), now(), now(), NULL \
              FROM messaging_conversations WHERE security_profile = 'PRIVATE_E2EE' LIMIT 1")
        .await
        .is_err());
}

#[tokio::test]
async fn a_private_e2ee_conversation_can_never_become_managed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, _) = h.pair(&c).await;
    let conv = h
        .create(&c, alice, json!({ "contextClass": "GROUP" }), &key())
        .await;
    let id = conv.json["conversationId"].clone();
    let r = h
        .patch_conv(
            &c,
            None,
            &id,
            json!({ "expectedVersion": 1, "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "SECURITY_PROFILE_IMMUTABLE")
    );
    // Not even directly in the database.
    let sql = format!(
        "UPDATE messaging_conversations SET security_profile = 'MANAGED', protocol = 'CFMEET_MANAGED_V1', ciphersuite = NULL, \
         mls_group_id = NULL, version = version + 1 WHERE id = '{}'",
        id.as_str().unwrap()
    );
    let err = h.sql(&sql).await.unwrap_err();
    assert!(err.contains("security profile is immutable"), "{err}");
    // Changing the policy later does not touch existing conversations.
    h.put_policy(
        &c,
        0,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED"] } } }),
    )
    .await;
    let r = h
        .get(
            &c,
            None,
            &format!("/v1/conversations/{}", id.as_str().unwrap()),
        )
        .await;
    assert_eq!(r.json["securityProfile"], "PRIVATE_E2EE");
    assert_eq!(r.json["policy"], conv.json["policy"]);
}

#[tokio::test]
async fn every_context_class_follows_its_persisted_policy() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, bob) = h.pair(&c).await;
    // Every context — SUPPORT included — defaults to PRIVATE_E2EE (Appendix G).
    let r = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "SUPPORT", "context": { "namespace": "cases", "reference": "case:1" } }),
            &key(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["securityProfile"], "PRIVATE_E2EE");
    assert_eq!(r.json["policy"]["platformPolicyVersion"], 2);
    let r = h
        .create(&c, alice, json!({ "contextClass": "ORDER" }), &key())
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    h.put_policy(
        &c,
        0,
        json!({ "contexts": {
            "MEETING": { "enabled": false }
        } }),
    )
    .await;
    let r = h
        .create(&c, alice, json!({ "contextClass": "MEETING" }), &key())
        .await;
    assert_eq!(r.code(), "CONTEXT_NOT_ALLOWED");
    for (class, extra) in [
        ("DIRECT", json!({ "counterpartPrincipalId": bob })),
        ("GROUP", json!({})),
        (
            "TRANSACTION",
            json!({ "context": { "namespace": "orders", "reference": "ord_1" } }),
        ),
        (
            "CONSULTATION",
            json!({ "context": { "namespace": "consultations", "reference": "c-77", "version": 2 } }),
        ),
        (
            "SUPPORT",
            json!({ "context": { "namespace": "cases", "reference": "case:9" } }),
        ),
        ("ENTERPRISE", json!({})),
        (
            "DEVELOPER_DEFINED",
            json!({ "context": { "namespace": "my-app", "reference": "room/7" } }),
        ),
    ] {
        let mut body = json!({ "contextClass": class });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let r = h.create(&c, alice, body, &key()).await;
        assert_eq!(r.status, StatusCode::CREATED, "{class}: {}", r.text);
        assert_eq!(r.json["contextClass"], class);
        assert_eq!(r.json["policy"]["applicationPolicyVersion"], 1);
    }
    // MEETING re-enabled later works too (eight classes).
    h.put_policy(&c, 1, json!({})).await;
    assert_eq!(
        h.create(&c, alice, json!({ "contextClass": "MEETING" }), &key())
            .await
            .status,
        StatusCode::CREATED
    );
    assert_eq!(
        h.scalar("SELECT count(DISTINCT context_class) FROM messaging_conversations")
            .await,
        8,
        "all eight context classes"
    );
    // A context allowing only MANAGED has no default: the creator must choose it
    // explicitly — never a fallback from PRIVATE_E2EE to MANAGED.
    let r = h
        .put_policy(
            &c,
            2,
            json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED"] } } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = h
        .create(&c, alice, json!({ "contextClass": "GROUP" }), &key())
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("SECURITY_PROFILE_NOT_ALLOWED", "NO_DEFAULT_PROFILE")
    );
    let r = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "GROUP", "securityProfile": "PRIVATE_E2EE" }),
            &key(),
        )
        .await;
    assert_eq!(r.code(), "SECURITY_PROFILE_NOT_ALLOWED");
}

#[tokio::test]
async fn context_references_are_unique_opaque_and_never_authority() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, bob) = h.pair(&c).await;
    let body = json!({ "contextClass": "TRANSACTION", "context": { "namespace": "orders", "reference": "ord_42" } });
    let first = h.create(&c, alice, body.clone(), &key()).await;
    assert_eq!(first.status, StatusCode::CREATED);
    // The same live context returns the same conversation (not a second one) …
    let again = h.create(&c, alice, body.clone(), &key()).await;
    assert_eq!(
        (again.status, &again.json["conversationId"]),
        (StatusCode::OK, &first.json["conversationId"])
    );
    // … but knowing the reference grants nothing: bob is not a member.
    let r = h
        .get(
            &c,
            Some(bob),
            &format!(
                "/v1/conversations/{}",
                first.json["conversationId"].as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Another namespace, class or tenant is independent.
    let ns = h.create(&c, alice, json!({ "contextClass": "TRANSACTION", "context": { "namespace": "trips", "reference": "ord_42" } }), &key()).await;
    assert_eq!(ns.status, StatusCode::CREATED);
    let other = h.customer(ALL);
    let (oa, _) = h.pair(&other).await;
    assert_eq!(
        h.create(&other, oa, body.clone(), &key()).await.status,
        StatusCode::CREATED
    );
    // A context held by a conversation of another profile is a conflict, not a merge.
    h.put_policy(&c, 0, json!({ "contexts": { "TRANSACTION": { "allowedProfiles": ["PRIVATE_E2EE", "MANAGED"] } } })).await;
    let mut managed = body.clone();
    managed["securityProfile"] = json!("MANAGED");
    let r = h.create(&c, alice, managed, &key()).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("CONFLICT", "CONTEXT_HELD_BY_OTHER_PROFILE")
    );
    // Unsafe references are refused.
    for bad in [
        json!({ "namespace": "Orders", "reference": "x" }),
        json!({ "namespace": "orders", "reference": "a b" }),
        json!({ "namespace": "orders", "reference": "x", "grantAccess": true }),
    ] {
        let r = h
            .create(
                &c,
                alice,
                json!({ "contextClass": "TRANSACTION", "context": bad }),
                &key(),
            )
            .await;
        assert_eq!(r.code(), "VALIDATION_ERROR", "{bad}");
    }
}

#[tokio::test]
async fn creation_is_idempotent_per_key() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, _) = h.pair(&c).await;
    let k = key();
    let r = h
        .send(
            Method::POST,
            Some(&c.token),
            Some(alice),
            "/v1/conversations",
            Some(json!({ "contextClass": "GROUP" })),
            &[],
        )
        .await;
    assert_eq!(r.code(), "IDEMPOTENCY_KEY_REQUIRED");
    let first = h
        .create(&c, alice, json!({ "contextClass": "GROUP" }), &k)
        .await;
    assert_eq!(first.status, StatusCode::CREATED);
    let replay = h
        .create(&c, alice, json!({ "contextClass": "GROUP" }), &k)
        .await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.json, first.json);
    let reused = h
        .create(&c, alice, json!({ "contextClass": "ENTERPRISE" }), &k)
        .await;
    assert_eq!(reused.code(), "IDEMPOTENCY_KEY_REUSED");
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_conversations")
            .await,
        1
    );
    assert_eq!(
        h.create(&c, alice, json!({ "contextClass": "GROUP" }), "short")
            .await
            .code(),
        "VALIDATION_ERROR"
    );
}

#[tokio::test]
async fn lifecycle_transitions_are_server_authoritative_and_versioned() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, _) = h.pair(&c).await;
    let conv = h
        .create(&c, alice, json!({ "contextClass": "GROUP" }), &key())
        .await;
    let id = conv.json["conversationId"].clone();
    let step = |v: i64, s: &str| json!({ "expectedVersion": v, "lifecycleState": s, "reason": "TEST_STEP" });
    let r = h.patch_conv(&c, None, &id, step(1, "LOCKED")).await;
    assert_eq!(
        (r.status, &r.json["lifecycleState"], &r.json["version"]),
        (StatusCode::OK, &json!("LOCKED"), &json!(2)),
        "{}",
        r.text
    );
    conforms(
        "messaging/common/v1.schema.json#/$defs/Conversation",
        &r.json,
    );
    // Stale version.
    assert_eq!(
        h.patch_conv(&c, None, &id, step(1, "ACTIVE")).await.code(),
        "VERSION_CONFLICT"
    );
    // EXPIRED is the server's; invalid transitions are refused.
    let r = h.patch_conv(&c, None, &id, step(2, "EXPIRED")).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("INVALID_TRANSITION", "SERVER_ONLY_STATE")
    );
    assert_eq!(
        h.patch_conv(&c, None, &id, step(2, "ARCHIVED"))
            .await
            .status,
        StatusCode::OK
    );
    let r = h.patch_conv(&c, None, &id, step(3, "LOCKED")).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("INVALID_TRANSITION", "TRANSITION_NOT_ALLOWED")
    );
    assert_eq!(
        h.patch_conv(&c, None, &id, step(3, "DELETED")).await.json["lifecycleState"],
        "DELETED"
    );
    assert_eq!(
        h.patch_conv(&c, None, &id, step(4, "ACTIVE")).await.code(),
        "INVALID_TRANSITION"
    );
    // The database refuses it as well, and nothing is ever physically deleted.
    assert!(h.sql(&format!("UPDATE messaging_conversations SET lifecycle_state = 'ACTIVE', version = version + 1 WHERE id = '{}'", id.as_str().unwrap())).await.is_err());
    assert!(h
        .sql(&format!(
            "DELETE FROM messaging_conversations WHERE id = '{}'",
            id.as_str().unwrap()
        ))
        .await
        .is_err());
    // One event per change, all conforming to the event contract.
    let events: Vec<Value> = h
        .conn()
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT jsonb_build_object('event_type', event_type, 'payload', payload) FROM messaging_outbox_events WHERE aggregate_id = '{}' ORDER BY sequence", id.as_str().unwrap()),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<Value>(0).unwrap())
        .collect();
    assert_eq!(events.len(), 4, "created + 3 transitions");
    for e in &events {
        conforms("messaging/events/v1.schema.json", e);
    }
    assert_eq!(events[3]["payload"]["lifecycle_state"], "DELETED");
}

use sea_orm::ConnectionTrait;

#[tokio::test]
async fn concurrent_lifecycle_updates_on_one_version_have_one_winner() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, _) = h.pair(&c).await;
    let conv = h
        .create(&c, alice, json!({ "contextClass": "GROUP" }), &key())
        .await;
    let id: Uuid = uuid(&conv.json["conversationId"]);
    let mut tasks = Vec::new();
    for target in [
        "LOCKED", "CLOSED", "ARCHIVED", "LOCKED", "CLOSED", "ARCHIVED",
    ] {
        let state = h.state.clone();
        let token = c.token.clone();
        tasks.push(tokio::spawn(async move {
            use tower::ServiceExt;
            let req = axum::http::Request::patch(format!("/v1/conversations/{id}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({ "expectedVersion": 1, "lifecycleState": target }).to_string(),
                ))
                .unwrap();
            messaging::http::app(state)
                .oneshot(req)
                .await
                .unwrap()
                .status()
        }));
    }
    let mut ok = 0;
    let mut conflicts = 0;
    for t in tasks {
        match t.await.unwrap() {
            StatusCode::OK => ok += 1,
            StatusCode::CONFLICT => conflicts += 1,
            s => panic!("{s}"),
        }
    }
    assert_eq!((ok, conflicts), (1, 5));
    assert_eq!(
        h.scalar(&format!(
            "SELECT version FROM messaging_conversations WHERE id = '{id}'"
        ))
        .await,
        2
    );
}

#[tokio::test]
async fn reopening_and_expiry_follow_the_conversations_policy_snapshot() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, _) = h.pair(&c).await;
    h.put_policy(&c, 0, json!({ "contexts": { "GROUP": { "reopenAllowed": false }, "TRANSACTION": { "maxLifetimeSeconds": 3600 } } })).await;
    let g = h
        .create(&c, alice, json!({ "contextClass": "GROUP" }), &key())
        .await;
    let gid = g.json["conversationId"].clone();
    h.patch_conv(
        &c,
        None,
        &gid,
        json!({ "expectedVersion": 1, "lifecycleState": "CLOSED" }),
    )
    .await;
    let r = h
        .patch_conv(
            &c,
            None,
            &gid,
            json!({ "expectedVersion": 2, "lifecycleState": "ACTIVE" }),
        )
        .await;
    assert_eq!(r.reason(), "REOPEN_NOT_ALLOWED");
    // Context expiry: bounded by policy, enforced by the server's clock.
    let r = h.create(&c, alice, json!({ "contextClass": "TRANSACTION", "expiresAt": (h.now() + chrono::Duration::hours(5)).to_rfc3339() }), &key()).await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    let t = h.create(&c, alice, json!({ "contextClass": "TRANSACTION", "context": { "namespace": "trips", "reference": "t1" } }), &key()).await;
    assert!(t.json["expiresAt"].is_string());
    let t2 = h.create(&c, alice, json!({ "contextClass": "TRANSACTION", "context": { "namespace": "trips", "reference": "t2" } }), &key()).await;
    h.clock.advance(chrono::Duration::hours(2));
    // Expiry during an update: the update is refused and the expiry is committed.
    let r = h
        .patch_conv(
            &c,
            None,
            &t.json["conversationId"],
            json!({ "expectedVersion": 1, "lifecycleState": "LOCKED" }),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("CONVERSATION_NOT_ACTIVE", "EXPIRED")
    );
    let tid = t.json["conversationId"].as_str().unwrap();
    assert_eq!(
        h.get(&c, None, &format!("/v1/conversations/{tid}"))
            .await
            .json["lifecycleState"],
        "EXPIRED"
    );
    // The worker expires the rest, once.
    assert_eq!(
        messaging::conversations::service::expire_due(&h.state, 100)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        messaging::conversations::service::expire_due(&h.state, 100)
            .await
            .unwrap(),
        0
    );
    let t2id = t2.json["conversationId"].as_str().unwrap();
    assert_eq!(
        h.get(&c, None, &format!("/v1/conversations/{t2id}"))
            .await
            .json["lifecycleReason"],
        "CONTEXT_EXPIRED"
    );
    // An expired context frees its reference for a new conversation.
    let again = h.create(&c, alice, json!({ "contextClass": "TRANSACTION", "context": { "namespace": "trips", "reference": "t1" } }), &key()).await;
    assert_eq!(again.status, StatusCode::CREATED);
}

#[tokio::test]
async fn conversations_are_isolated_by_tenant_application_environment_and_membership() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, bob) = h.pair(&c).await;
    let carol = h.principal(&c);
    let conv = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "DIRECT", "counterpartPrincipalId": bob }),
            &key(),
        )
        .await;
    let id = conv.json["conversationId"].as_str().unwrap().to_string();
    let path = format!("/v1/conversations/{id}");
    // Members see it; a non-member principal of the same environment does not.
    assert_eq!(h.get(&c, Some(bob), &path).await.status, StatusCode::OK);
    assert_eq!(
        h.get(&c, Some(carol), &path).await.status,
        StatusCode::NOT_FOUND
    );
    // Another tenant, another application of the same tenant, another environment.
    let other_tenant = h.customer(ALL);
    let mut other_app = c.context.clone();
    other_app.scope.application_id = Uuid::new_v4();
    other_app.credential_id = Uuid::new_v4();
    let other_app = h.register_token(other_app);
    let mut other_env = c.context.clone();
    other_env.scope.environment_id = Uuid::new_v4();
    other_env.credential_id = Uuid::new_v4();
    let other_env = h.register_token(other_env);
    for cust in [&other_tenant, &other_app, &other_env] {
        assert_eq!(h.get(cust, None, &path).await.status, StatusCode::NOT_FOUND);
        assert_eq!(
            h.patch_conv(
                cust,
                None,
                &json!(id),
                json!({ "expectedVersion": 1, "lifecycleState": "LOCKED" })
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            h.get(cust, None, "/v1/conversations").await.json["conversations"],
            json!([])
        );
    }
    // Lists: a member's own conversations; the administration view needs its scope.
    assert_eq!(
        h.get(&c, Some(carol), "/v1/conversations").await.json["conversations"],
        json!([])
    );
    let reader = h.credential(&c, &["messages.read"]);
    assert_eq!(
        h.get(&reader, None, "/v1/conversations").await.code(),
        "INSUFFICIENT_SCOPE"
    );
    assert_eq!(
        h.get(&reader, Some(alice), "/v1/conversations").await.json["conversations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Unauthorized scopes cannot create or administer.
    assert_eq!(
        h.create(&reader, alice, json!({ "contextClass": "GROUP" }), &key())
            .await
            .code(),
        "INSUFFICIENT_SCOPE"
    );
    let creator = h.credential(&c, &["conversations.create"]);
    assert_eq!(
        h.patch_conv(
            &creator,
            None,
            &json!(id),
            json!({ "expectedVersion": 1, "lifecycleState": "LOCKED" })
        )
        .await
        .code(),
        "INSUFFICIENT_SCOPE"
    );
    // A principal administering must own the conversation.
    let r = h
        .patch_conv(
            &c,
            Some(bob),
            &json!(id),
            json!({ "expectedVersion": 1, "lifecycleState": "LOCKED" }),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("PRINCIPAL_NOT_AUTHORIZED", "NOT_OWNER")
    );
    // An inactive acting principal is refused.
    h.set_principal(&c, alice, PrincipalStatus::Inactive);
    assert_eq!(
        h.get(&c, Some(alice), &path).await.reason(),
        "PRINCIPAL_INACTIVE"
    );
}

#[tokio::test]
async fn policy_changes_are_versioned_validated_and_audited() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, _) = h.pair(&c).await;
    let v = h.get(&c, None, "/v1/messaging/policy").await;
    assert_eq!(v.status, StatusCode::OK, "{}", v.text);
    conforms("messaging/common/v1.schema.json#/$defs/PolicyView", &v.json);
    assert_eq!(v.json["version"], 0);
    let first = h
        .put_policy(
            &c,
            0,
            json!({ "contexts": { "GROUP": { "maxMembers": 50 } } }),
        )
        .await;
    assert_eq!(first.json["version"], 1);
    // A stale writer loses.
    assert_eq!(
        h.put_policy(&c, 0, json!({})).await.code(),
        "POLICY_VERSION_CONFLICT"
    );
    // Invalid documents are refused.
    for bad in [
        json!({ "contexts": { "DIRECT": { "maxMembers": 10 } } }),
        json!({ "contexts": { "GROUP": { "defaultProfile": "MANAGED" } } }),
        json!({ "contexts": { "ORDER": {} } }),
        json!({ "tenantId": "x" }),
    ] {
        assert!(
            h.put_policy(&c, 1, bad.clone())
                .await
                .status
                .is_client_error(),
            "{bad}"
        );
    }
    // Creation can pin the policy version it decided on.
    let r = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "GROUP", "expectedPolicyVersion": 0 }),
            &key(),
        )
        .await;
    assert_eq!(r.code(), "POLICY_VERSION_CONFLICT");
    let r = h
        .create(
            &c,
            alice,
            json!({ "contextClass": "GROUP", "expectedPolicyVersion": 1 }),
            &key(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    // Audited, evented, and the snapshot a conversation references is immutable.
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_audit WHERE action = 'POLICY_UPDATED'")
            .await,
        1
    );
    assert_eq!(h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE event_type = 'messaging.policy.updated'").await, 1);
    assert!(h
        .sql("UPDATE messaging_policy_snapshots SET document = '{}'")
        .await
        .is_err());
    // Policy administration needs conversations.manage.
    let sender = h.credential(&c, &["messages.send"]);
    assert_eq!(
        h.get(&sender, None, "/v1/messaging/policy").await.code(),
        "INSUFFICIENT_SCOPE"
    );
}

#[tokio::test]
async fn listing_is_keyset_paginated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, _) = h.pair(&c).await;
    for _ in 0..5 {
        h.create(&c, alice, json!({ "contextClass": "GROUP" }), &key())
            .await;
        h.clock.advance(chrono::Duration::milliseconds(5));
    }
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            None => "/v1/conversations?limit=2".to_string(),
            Some(c) => format!("/v1/conversations?limit=2&cursor={c}"),
        };
        let page = h.get(&c, Some(alice), &path).await;
        conforms(
            "messaging/common/v1.schema.json#/$defs/ConversationPage",
            &page.json,
        );
        for conv in page.json["conversations"].as_array().unwrap() {
            seen.push(conv["conversationId"].as_str().unwrap().to_string());
        }
        match page.json["nextCursor"].as_str() {
            Some(n) => cursor = Some(n.to_string()),
            None => break,
        }
    }
    let unique: std::collections::BTreeSet<_> = seen.iter().collect();
    assert_eq!((seen.len(), unique.len()), (5, 5));
    assert_eq!(
        h.get(&c, Some(alice), "/v1/conversations?limit=0")
            .await
            .code(),
        "VALIDATION_ERROR"
    );
    assert_eq!(
        h.get(&c, Some(alice), "/v1/conversations?cursor=%%%")
            .await
            .code(),
        "VALIDATION_ERROR"
    );
}

#[tokio::test]
async fn message_sequences_are_unique_and_gap_free_under_concurrency() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL);
    let (alice, _) = h.pair(&c).await;
    let conv = h
        .create(&c, alice, json!({ "contextClass": "GROUP" }), &key())
        .await;
    let id = uuid(&conv.json["conversationId"]);
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let db = h.conn().clone();
        tasks.push(tokio::spawn(async move {
            use sea_orm::TransactionTrait;
            let txn = db.begin().await.unwrap();
            let n = messaging::conversations::sequence::allocate_sequence(&txn, id)
                .await
                .unwrap();
            txn.commit().await.unwrap();
            n
        }));
    }
    let mut got = Vec::new();
    for t in tasks {
        got.push(t.await.unwrap());
    }
    got.sort();
    assert_eq!(got, (1..=20).collect::<Vec<_>>());
    // A rolled-back allocation does not consume a number.
    {
        use sea_orm::TransactionTrait;
        let txn = h.conn().begin().await.unwrap();
        assert_eq!(
            messaging::conversations::sequence::allocate_sequence(&txn, id)
                .await
                .unwrap(),
            21
        );
        txn.rollback().await.unwrap();
    }
    use sea_orm::TransactionTrait;
    let txn = h.conn().begin().await.unwrap();
    assert_eq!(
        messaging::conversations::sequence::allocate_sequence(&txn, id)
            .await
            .unwrap(),
        21
    );
    txn.commit().await.unwrap();
    assert!(h.sql(&format!("UPDATE messaging_conversation_sequences SET next_sequence = 1 WHERE conversation_id = '{id}'")).await.is_err());
    assert!(
        messaging::conversations::sequence::allocate_sequence(h.conn(), Uuid::new_v4())
            .await
            .is_err()
    );
}
