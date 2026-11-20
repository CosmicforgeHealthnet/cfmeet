//! Owner events (A7) through the real event listener (cf-service+jwt, trust bundle,
//! single-use tokens) and the HQ feed / acknowledgements through the real HQ chain.

mod common;

use std::sync::Arc;

use admin_bridge::events::{run_events, EventIngress, EVENTS_PATH};
use admin_bridge::store::{OwnerEvent, Store};
use cf_service_auth::{
    services as s, KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId,
    ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl,
    TrustPolicy,
};
use chrono::{Duration, Utc};
use common::*;
use serde_json::{json, Value};

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;

struct Producers {
    developer: ServiceTokenSigner,
    calling: ServiceTokenSigner,
    organizations: ServiceTokenSigner,
    addr: std::net::SocketAddr,
}

fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

fn signer(id: ServiceId, k: ServiceSigningKey) -> ServiceTokenSigner {
    ServiceTokenSigner::new(id, ENV, k, TokenTtl::default(), Arc::new(SystemClock))
}

/// Starts the event listener for `h.bridge` with a trust bundle trusting two producers;
/// Organizations signs with a key the bundle does not trust.
async fn listener(h: &Harness) -> Producers {
    let (dev, call, org) = (key("dev-test-1"), key("calling-test-1"), key("org-test-1"));
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str]| {
        json!({"issuer": id, "audience": s::ADMIN_BRIDGE, "runtime_environment": ENV, "keys": [k.public_jwk()], "permissions": perms})
    };
    let bundle = json!({
        "bundle_version": 1,
        "receiver": s::ADMIN_BRIDGE,
        "runtime_environment": ENV,
        "trusted_issuers": [
            issuer(s::DEVELOPER_PLATFORM, &dev, &["hq-bridge-events.publish"]),
            issuer(s::CALLING, &call, &["hq-bridge-events.publish"]),
        ],
    })
    .to_string();
    let policy = TrustPolicy::from_json(&bundle, &ServiceId::admin_bridge(), ENV).unwrap();
    let ingress = Arc::new(EventIngress::new(ServiceTokenVerifier::new(
        policy,
        Arc::new(SystemClock),
    )));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(run_events(h.bridge.clone(), l, ingress));
    Producers {
        developer: signer(ServiceId::developer_platform(), dev),
        calling: signer(ServiceId::calling(), call),
        organizations: signer(ServiceId::organizations(), org),
        addr,
    }
}

fn signed(signer: &ServiceTokenSigner) -> reqwest::header::HeaderMap {
    signer
        .sign_request(
            &OutboundServiceRequest::new_operation(ServiceId::admin_bridge()),
            &http::Method::POST,
            EVENTS_PATH,
        )
        .unwrap()
        .headers()
}

async fn post(p: &Producers, headers: reqwest::header::HeaderMap, body: &Value) -> (u16, Value) {
    let r = reqwest::Client::new()
        .post(format!("http://{}{EVENTS_PATH}", p.addr))
        .headers(headers)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(body).unwrap())
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, serde_json::from_slice(&r.bytes().await.unwrap()).unwrap())
}

fn event(id: uuid::Uuid, kind: &str, subject: &str, state: &str, version: i64) -> Value {
    json!({
        "event_id": id.to_string(), "type": kind,
        "subject": {"kind": "verification_case", "id": subject},
        "state": state, "version": version,
        "occurred_at": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

#[tokio::test]
async fn owner_events_are_deduplicated_versioned_and_quarantined() {
    let h = start(Options::default()).await;
    let p = listener(&h).await;
    const CASE: &str = "verification.case.changed.v1";
    let first = uuid::Uuid::new_v4();
    let (status, v) = post(&p, signed(&p.developer), &event(first, CASE, "case-1", "OPEN", 2)).await;
    assert_eq!((status, v["status"].as_str()), (202, Some("APPENDED")), "{v}");
    assert_eq!(v["sequence"], 1);
    // The same event again (a new token): a duplicate, nothing changes.
    let (_, v) = post(&p, signed(&p.developer), &event(first, CASE, "case-1", "OPEN", 2)).await;
    assert_eq!(v["status"], "DUPLICATE");
    // Not newer for the same subject: accepted, not appended. Newer: appended.
    let (_, v) = post(
        &p,
        signed(&p.developer),
        &event(uuid::Uuid::new_v4(), CASE, "case-1", "CLAIMED", 2),
    )
    .await;
    assert_eq!(v["status"], "STALE");
    let (_, v) = post(
        &p,
        signed(&p.developer),
        &event(uuid::Uuid::new_v4(), CASE, "case-1", "APPROVED", 3),
    )
    .await;
    assert_eq!((v["status"].as_str(), v["sequence"].as_i64()), (Some("APPENDED"), Some(2)));
    // Calling does not publish verification cases; nobody publishes an unknown type.
    let (_, v) = post(
        &p,
        signed(&p.calling),
        &event(uuid::Uuid::new_v4(), CASE, "case-9", "OPEN", 1),
    )
    .await;
    assert_eq!(v["status"], "QUARANTINED");
    let (_, v) = post(
        &p,
        signed(&p.developer),
        &event(uuid::Uuid::new_v4(), "workspace.renamed.v1", "w-1", "OPEN", 1),
    )
    .await;
    assert_eq!(v["status"], "QUARANTINED");
    // A token is single use.
    let once = signed(&p.calling);
    let body = event(uuid::Uuid::new_v4(), "abuse.report.changed.v1", "r-1", "OPEN", 1);
    assert_eq!(post(&p, once.clone(), &body).await.1["status"], "APPENDED");
    assert_eq!(post(&p, once, &body).await.0, 401);
    // Not trusted; no token; an open body (personal data is never accepted).
    assert_eq!(
        post(&p, signed(&p.organizations), &body).await.0,
        401
    );
    assert_eq!(
        post(&p, reqwest::header::HeaderMap::new(), &body).await.0,
        401
    );
    let mut open = event(uuid::Uuid::new_v4(), CASE, "case-2", "OPEN", 1);
    open["email"] = json!("someone@example.com");
    assert_eq!(post(&p, signed(&p.developer), &open).await.0, 400);

    let feed = h
        .store
        .event_feed(Utc::now() - Duration::hours(1), Utc::now() + Duration::minutes(1), None, 10)
        .await
        .unwrap();
    assert_eq!(
        feed.iter().map(|f| (f.sequence, f.event.state.as_str(), f.event.producer.as_str())).collect::<Vec<_>>(),
        [(1, "OPEN", "developer-platform"), (2, "APPROVED", "developer-platform"), (3, "OPEN", "calling")]
    );
    let inbox = h
        .store
        .projections()
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.name == "events.inbox")
        .unwrap();
    assert_eq!(inbox.quarantined, 2);
    assert!(inbox.last_event_at.is_some());
}

fn worker(path: &str, query: &str, permission: &str, sub: &str) -> Call {
    let mut c = call(path, query, permission, &["cfmeet.source_sync"]);
    c.acr = "urn:cosmicforge:acr:workload";
    c.sub = sub.into();
    c
}

#[tokio::test]
async fn the_feed_pages_in_sequence_and_acknowledgements_never_move_back() {
    let h = start(Options::default()).await;
    for (n, state) in ["OPEN", "CLAIMED", "APPROVED"].iter().enumerate() {
        let e = OwnerEvent {
            producer: "developer-platform".into(),
            event_id: uuid::Uuid::new_v4(),
            event_type: "verification.case.changed.v1".into(),
            subject_kind: "verification_case".into(),
            subject_id: "case-1".into(),
            state: state.to_string(),
            version: n as i64 + 1,
            occurred_at: Utc::now(),
        };
        h.store.ingest_event(&e, true, Utc::now()).await.unwrap();
    }
    let start_t = (Utc::now() - Duration::hours(1)).format("%Y-%m-%dT%H:%M:%SZ");
    let end_t = (Utc::now() + Duration::seconds(30)).format("%Y-%m-%dT%H:%M:%SZ");
    let feed = |cursor: &str| {
        worker(
            "/hq/events",
            &format!("{}&limit=2{cursor}&start={start_t}&end={end_t}", scoped("event-sync")),
            "hq.events.read",
            "hq-worker",
        )
    };
    let (status, v) = send(&h, &feed("")).await;
    assert_eq!(status, 200, "{v}");
    assert_envelope(&v, true);
    let items = v["result"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["sequence"], 1);
    assert_eq!(items[0]["subject"], json!({"kind": "verification_case", "id": "case-1"}));
    assert_eq!(items[1]["state"], "CLAIMED");
    assert_eq!(v["result"]["next_cursor"], "2");
    let (_, v) = send(&h, &feed("&cursor=2")).await;
    assert_eq!(v["result"]["items"][0]["sequence"], 3);
    assert!(v["result"]["next_cursor"].is_null());
    // Only the worker's feed identity reads the feed.
    let mut staff = feed("");
    staff.roles = vec!["cfmeet.platform_ops"];
    staff.acr = "urn:cosmicforge:acr:mfa";
    staff.sub = "staff-0009".into();
    assert_ne!(send(&h, &staff).await.0, 200);

    let ack = |sequence: i64| {
        let mut c = worker(
            "/hq/event-acknowledgements",
            &scoped("event-sync"),
            "hq.events.ack",
            "hq-worker",
        );
        c.method = "POST";
        c.body = serde_json::to_vec(&json!({"consumer": "hq-cfmeet-admin", "sequence": sequence}))
            .unwrap();
        c
    };
    let (status, v) = send(&h, &ack(2)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v["result"],
        json!({"consumer": "hq-cfmeet-admin", "acknowledged_sequence": 2})
    );
    // Lower: the stored one. Beyond the head: refused, nothing changed.
    assert_eq!(send(&h, &ack(1)).await.1["result"]["acknowledged_sequence"], 2);
    assert_eq!(code(&send(&h, &ack(9)).await.1), "COMMAND_BODY_INVALID");
}
