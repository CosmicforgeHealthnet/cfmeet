//! ADR 0068 A4 — `partners.workspace.register-first-party` relayed by the HQ admin bridge
//! with the OWNER-DELIVERED secret rule (owner decision with HQ, 2026-11-21), through the
//! REAL router (CI: real PostgreSQL): the one-time owner invitation is sealed for the
//! Notification Service and delivered only to the invited address as a single-use claim
//! link; HQ gets a reference (deliveryRef, masked address, expiry) and never the token,
//! the link or the address.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use common::*;
use developer_platform::{
    control::owner_delivery::{self, OwnerDelivery},
    notification_publications::{IngestResult, NotificationEventSink, PublishFailure},
};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";
const PATH: &str = "/internal/v1/operator/organizations";
const OPERATOR: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const APPROVER: (&str, &str) = ("x-cf-approver", "hq:staff-0002");
const APPROVAL: (&str, &str) = ("x-cf-approval-ref", "appr-1");
const DIGEST: (&str, &str) = (
    "x-cf-intent-digest",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
);
const OWNER: &str = "owner.person@first.example.com";

fn delivery() -> OwnerDelivery {
    let (public, _private) =
        cf_notification_client::seal::generate_keypair("notif-hpke-2026-11").unwrap();
    OwnerDelivery {
        sealing_key: cf_notification_client::SealingKey::parse(&public).unwrap(),
    }
}

/// I1 answering every event with `outcome`.
struct Sink {
    outcome: &'static str,
    seen: Mutex<Vec<Value>>,
}

#[async_trait]
impl NotificationEventSink for Sink {
    async fn ingest(&self, events: &[Value]) -> Result<Vec<IngestResult>, PublishFailure> {
        self.seen.lock().unwrap().extend(events.iter().cloned());
        Ok(events
            .iter()
            .map(|e| IngestResult {
                event_id: e["eventId"].as_str().unwrap().to_string(),
                outcome: self.outcome.to_string(),
                reason: None,
            })
            .collect())
    }
}

fn no_secret(text: &str) {
    assert!(!text.contains(OWNER), "never the address: {text}");
    assert!(!text.contains("owner.person"), "never the address: {text}");
    assert!(!text.contains("cfinv_"), "never the token: {text}");
    assert!(!text.contains("/invitations/"), "never a link: {text}");
}

#[tokio::test]
async fn hq_registers_a_first_party_workspace_and_the_owner_alone_gets_the_claim_link() {
    let Some(c) = Control::with_state(|s| s.with_owner_delivery(delivery())).await else {
        return;
    };
    let tenant = c.pa.first_party_tenant();
    let key = Uuid::new_v4().to_string();
    let body = json!({ "tenantId": tenant, "displayName": "CF Meet Labs", "ownerEmail": OWNER });
    let two = [
        OPERATOR,
        APPROVER,
        APPROVAL,
        DIGEST,
        ("idempotency-key", key.as_str()),
    ];
    // R4: no approval → refused; no key → refused; nothing done.
    let r = c
        .bridge_send(
            Method::POST,
            PATH,
            &[OPERATOR, ("idempotency-key", &key)],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    let r = c
        .bridge_send(
            Method::POST,
            PATH,
            &[OPERATOR, APPROVER, APPROVAL, DIGEST],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_organizations").await,
        0
    );

    // Applied once: a reference, never the secret.
    let r = c
        .bridge_send(Method::POST, PATH, &two, Some(body.clone()))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}AdminCommandResult"), &r.json);
    conforms(
        &format!("{CTRL}FirstPartyRegistrationResource"),
        &r.json["resource"],
    );
    assert_eq!(r.json["status"], "INVITED");
    assert_eq!(r.json["version"], "1");
    let res = &r.json["resource"];
    assert_eq!(res["organizationId"], json!(tenant));
    assert_eq!(res["delivery"], "OWNER_DELIVERED");
    assert_eq!(res["deliveryChannel"], "CLAIM_LINK_EMAIL");
    assert_eq!(res["recipientMasked"], "o***@f***.com");
    assert_eq!(res["deliveryState"], "PENDING");
    assert_eq!(res["claimState"], "UNCLAIMED");
    assert!(res["expiresAt"].is_string());
    let delivery_ref = res["deliveryRef"].as_str().unwrap().to_string();
    no_secret(&r.json.to_string());

    // Nothing at rest carries the token or the address in clear: the invitation keeps a
    // digest, the delivery a SEALED envelope, the ledger and the activity ids only.
    for sql in [
        "SELECT row_to_json(o)::text FROM devplat_admin_operations o",
        "SELECT row_to_json(d)::text FROM devplat_owner_deliveries d",
        "SELECT summary::text FROM devplat_activity_events",
    ] {
        let rows =
            c.db.conn
                .query_all(sea_orm::Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    sql,
                ))
                .await
                .unwrap();
        assert!(!rows.is_empty(), "{sql}");
        for row in rows {
            no_secret(&row.try_get_by_index::<String>(0).unwrap());
        }
    }
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_owner_deliveries WHERE envelope ? 'sealed' AND delivery_state = 'PENDING'")
            .await,
        1
    );

    // Replay: the same workspace and delivery, never a second invitation.
    let again = c
        .bridge_send(Method::POST, PATH, &two, Some(body.clone()))
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.json);
    assert_eq!(again.json["resource"]["deliveryRef"], delivery_ref.as_str());
    assert_eq!(again.json["status"], "INVITED");
    assert_eq!(c.count("SELECT count(*) FROM devplat_invitations").await, 1);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_owner_deliveries")
            .await,
        1
    );
    // The same key for another subject is a conflict; another key for the same tenant is
    // refused (the workspace exists), nothing done.
    let other = c.pa.first_party_tenant();
    let r = c
        .bridge_send(
            Method::POST,
            PATH,
            &two,
            Some(json!({ "tenantId": other, "displayName": "Other", "ownerEmail": OWNER })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let key2 = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            PATH,
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &key2),
            ],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(c.count("SELECT count(*) FROM devplat_invitations").await, 1);

    // The operation record.
    let r = c
        .bridge(
            &format!("/internal/v1/operator/operations/{key}"),
            &[OPERATOR],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}AdminOperationRecord"), &r.json);
    assert_eq!(r.json["source_status"], "INVITED");
    assert_eq!(r.json["source_version"], "1");

    // HQ reads the delivery state afterwards (bridge only).
    let read = format!("/internal/v1/operator/owner-deliveries/{delivery_ref}");
    let r = c.bridge(&read, &[OPERATOR]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}OwnerDelivery"), &r.json);
    assert_eq!(r.json["deliveryState"], "PENDING");
    no_secret(&r.json.to_string());
    assert_eq!(
        c.operator(Method::GET, &read, None).await.status,
        StatusCode::NOT_FOUND
    );
    let r = c
        .bridge(
            &format!("/internal/v1/operator/owner-deliveries/{}", Uuid::new_v4()),
            &[OPERATOR],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // The worker relays the sealed event once; the sealed copy is dropped on settlement.
    let sink = Arc::new(Sink {
        outcome: "ACCEPTED",
        seen: Mutex::new(vec![]),
    });
    let report = owner_delivery::relay_due(&c.db.conn, sink.as_ref(), chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(report.sent, 1);
    {
        let seen = sink.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["eventType"], owner_delivery::OWNER_INVITATION_EMAIL);
        assert_eq!(seen[0]["producer"], "cfmeet.developer-platform");
        assert!(seen[0]["sealed"].is_object());
        no_secret(&seen[0].to_string());
    }
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_owner_deliveries WHERE envelope IS NULL AND delivery_state = 'SENT'")
            .await,
        1
    );
    let report = owner_delivery::relay_due(&c.db.conn, sink.as_ref(), chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(report, Default::default(), "never resent");
    let r = c.bridge(&read, &[OPERATOR]).await;
    assert_eq!(r.json["deliveryState"], "SENT");
    assert!(r.json["sentAt"].is_string());
}

#[tokio::test]
async fn a_refused_or_expired_delivery_is_never_resent() {
    let Some(c) = Control::with_state(|s| s.with_owner_delivery(delivery())).await else {
        return;
    };
    let mut refs = vec![];
    for _ in 0..2 {
        let tenant = c.pa.first_party_tenant();
        let key = Uuid::new_v4().to_string();
        let r = c
            .bridge_send(
                Method::POST,
                PATH,
                &[
                    OPERATOR,
                    APPROVER,
                    APPROVAL,
                    DIGEST,
                    ("idempotency-key", &key),
                ],
                Some(json!({ "tenantId": tenant, "displayName": "Labs", "ownerEmail": OWNER })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        refs.push(
            r.json["resource"]["deliveryRef"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    // The first link expires unsent (HQ re-issues the command; nothing is resent).
    c.db.conn
        .execute_unprepared(&format!(
            "UPDATE devplat_owner_deliveries SET expires_at = now() - interval '1 minute' WHERE id = '{}'",
            refs[0]
        ))
        .await
        .unwrap();
    let sink = Sink {
        outcome: "REJECTED",
        seen: Mutex::new(vec![]),
    };
    let report = owner_delivery::relay_due(&c.db.conn, &sink, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!((report.expired, report.failed), (1, 1));
    let state = |r: &str| {
        format!("SELECT count(*) FROM devplat_owner_deliveries WHERE id = '{r}' AND envelope IS NULL AND delivery_state = ")
    };
    assert_eq!(c.count(&(state(&refs[0]) + "'EXPIRED'")).await, 1);
    assert_eq!(c.count(&(state(&refs[1]) + "'FAILED'")).await, 1);
}

#[tokio::test]
async fn without_owner_delivery_the_bridge_is_refused_and_the_console_is_unchanged() {
    let Some(c) = Control::new().await else {
        return;
    };
    let tenant = c.pa.first_party_tenant();
    let key = Uuid::new_v4().to_string();
    let body = json!({ "tenantId": tenant, "displayName": "Labs", "ownerEmail": OWNER });
    let r = c
        .bridge_send(
            Method::POST,
            PATH,
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &key),
            ],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE");
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_organizations").await,
        0
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_admin_operations")
            .await,
        0
    );
    // The operator console keeps its own flow (the token handed to the console once).
    let r = c.operator(Method::POST, PATH, Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert!(r.json["ownerInvitation"]["token"]
        .as_str()
        .unwrap()
        .starts_with("cfinv_"));
    // Another service cannot claim an operator.
    let r = c
        .internal(&c.peers.messaging, Method::POST, PATH, Some(json!({})))
        .await;
    assert!(r.status == StatusCode::FORBIDDEN || r.status == StatusCode::UNAUTHORIZED);
}
