//! Provider event normalization (Part E §49, §123–§132), through the real HTTP callback
//! ingress: authentication before parsing, body limit, strict parser, provider-qualified
//! dedupe, out-of-order and late events, unknown provider objects, raw-payload hygiene,
//! and every event Telephony delivers to Calling validated against the canonical
//! `cfmeet.telephony.events` contract.

mod common;

use std::path::PathBuf;

use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Utc};
use common::{Harness, Response};
use jsonschema::{Registry, Resource, Validator};
use serde_json::{json, Value};
use telephony::domain::NormalizedEvent;
use uuid::Uuid;

const SIGNATURE: &str = "x-cfmeet-provider-signature";

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts")
}

fn read(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(contracts().join(rel)).unwrap()).unwrap()
}

/// The canonical Telephony -> Calling event contract (offline registry of every
/// registered JSON Schema contract).
fn events_contract() -> Validator {
    let manifest = read("manifest.json");
    let mut builder = Registry::new();
    for c in manifest["contracts"].as_array().unwrap() {
        if c["kind"] == "json-schema" {
            let doc = read(c["path"].as_str().unwrap());
            let id = doc["$id"].as_str().unwrap().to_string();
            builder = builder.add(id, Resource::from_contents(doc)).unwrap();
        }
    }
    let registry = builder.prepare().unwrap();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": "https://schemas.cfmeet.com/telephony/events/v1.schema.json",
        }))
        .unwrap()
}

fn assert_contract(events: &[Value]) {
    let v = events_contract();
    assert!(!events.is_empty());
    for e in events {
        let errors: Vec<String> = v.iter_errors(e).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{errors:?}\n{e:#}");
        let text = e.to_string();
        for forbidden in [
            "dp-a", "dp-b", "dp-", "cfm-", "+234", "provider", "sip", "carrier",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} leaked: {text}");
        }
    }
}

struct Callback {
    event_id: String,
    call_ref: Option<String>,
    correlation_key: Option<String>,
    event: &'static str,
    sequence: Option<i64>,
    at: DateTime<Utc>,
}

impl Callback {
    fn body(&self) -> Vec<u8> {
        let mut b = json!({
            "eventId": self.event_id,
            "event": self.event,
            "occurredAt": self.at,
        });
        if let Some(r) = &self.call_ref {
            b["callRef"] = json!(r);
        }
        if let Some(k) = &self.correlation_key {
            b["correlationKey"] = json!(k);
        }
        if let Some(s) = self.sequence {
            b["sequence"] = json!(s);
        }
        serde_json::to_vec(&b).unwrap()
    }
}

async fn post_raw(
    h: &Harness,
    provider: &str,
    body: Vec<u8>,
    signature: Option<String>,
) -> Response {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/provider-callbacks/{provider}"))
        .header("content-type", "application/json");
    if let Some(s) = signature {
        builder = builder.header(SIGNATURE, s);
    }
    let response = tower::ServiceExt::oneshot(
        h.app.clone(),
        builder.body(axum::body::Body::from(body)).unwrap(),
    )
    .await
    .unwrap();
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    Response {
        status,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text,
    }
}

async fn post(h: &Harness, cb: &Callback) -> Response {
    let body = cb.body();
    let sig = h.provider_a.sign(&body);
    post_raw(h, "dp-a", body, Some(sig)).await
}

async fn disposition(h: &Harness, event_id: &str) -> String {
    h.text(&format!(
        "SELECT disposition FROM provider_events WHERE provider_id = 'dp-a' AND provider_event_id = '{event_id}'"
    ))
    .await
    .unwrap()
}

/// An accepted, executed outbound leg: (telephony leg, provider call ref, correlation key).
async fn live_leg(h: &Harness) -> (Uuid, String, String) {
    let (leg, _) = h.originate().await;
    h.run().await;
    let correlation = h.correlation_of(leg).await;
    let call_ref = h.provider_a.call(&correlation).unwrap().provider_call_ref;
    (leg, call_ref, correlation)
}

fn cb(
    event_id: &str,
    call_ref: &str,
    event: &'static str,
    sequence: i64,
    at: DateTime<Utc>,
) -> Callback {
    Callback {
        event_id: event_id.into(),
        call_ref: Some(call_ref.into()),
        correlation_key: None,
        event,
        sequence: Some(sequence),
        at,
    }
}

#[tokio::test]
async fn callbacks_are_authenticated_size_limited_and_strictly_parsed_before_any_effect() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, call_ref, _) = live_leg(&h).await;
    let good = cb("auth-1", &call_ref, "ringing", 1, h.now());
    let before = h.scalar("SELECT count(*) FROM provider_events").await;
    // No signature, a wrong one, another provider's route, an unknown provider.
    let body = good.body();
    for (provider, sig) in [
        ("dp-a", None),
        ("dp-a", Some("v1=00".to_string())),
        ("dp-a", Some(format!("v1={}", "ab".repeat(32)))),
        ("no-such-provider", Some(h.provider_a.sign(&body))),
        ("DP-A", Some(h.provider_a.sign(&body))),
    ] {
        let r = post_raw(&h, provider, body.clone(), sig).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{provider} {}", r.text);
        assert_eq!(r.json["error"]["code"], "PROVIDER_EVENT_REJECTED");
    }
    // Oversized: refused before authentication or parsing.
    let huge = vec![b' '; 17 * 1024];
    let sig = h.provider_a.sign(&huge);
    let r = post_raw(&h, "dp-a", huge, Some(sig)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text);
    // Authenticated but not strictly well-formed: unknown field, unknown status.
    for body in [
        json!({ "eventId": "x-1", "event": "ringing", "occurredAt": h.now(), "callRef": call_ref, "carrierStatus": "180" }),
        json!({ "eventId": "x-2", "event": "early-media-ish", "occurredAt": h.now(), "callRef": call_ref }),
        json!({ "eventId": "x-3", "event": "ringing", "occurredAt": h.now(), "callRef": call_ref, "costMinor": 5 }),
    ] {
        let raw = serde_json::to_vec(&body).unwrap();
        let sig = h.provider_a.sign(&raw);
        let r = post_raw(&h, "dp-a", raw, Some(sig)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM provider_events").await,
        before
    );
    assert_eq!(h.leg_status(leg).await, "ORIGINATING");
    // Repeated authentication failures become an incident (no payload in it).
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'CALLBACK_AUTH_FAILURES'"
        )
        .await,
        0,
        "below the threshold"
    );
    for _ in 0..5 {
        post_raw(&h, "dp-a", body.clone(), None).await;
    }
    let evidence = h
        .text("SELECT restricted_evidence::text FROM telephony_incidents WHERE category = 'CALLBACK_AUTH_FAILURES'")
        .await
        .expect("incident raised");
    assert!(
        !evidence.contains("ringing") && !evidence.contains(&call_ref),
        "{evidence}"
    );
    // The genuine callback still works.
    assert_eq!(post(&h, &good).await.status, StatusCode::OK);
    assert_eq!(h.leg_status(leg).await, "RINGING");
}

#[tokio::test]
async fn provider_events_are_deduplicated_by_provider_and_event_id() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, call_ref, _) = live_leg(&h).await;
    let ringing = cb("dup-1", &call_ref, "ringing", 1, h.now());
    let (a, b) = tokio::join!(post(&h, &ringing), post(&h, &ringing));
    assert_eq!((a.status, b.status), (StatusCode::OK, StatusCode::OK));
    assert_eq!(post(&h, &ringing).await.status, StatusCode::OK);
    assert_eq!(
        h.scalar("SELECT count(*) FROM provider_events WHERE provider_event_id = 'dup-1'")
            .await,
        1
    );
    for _ in 0..3 {
        h.run().await;
    }
    assert_eq!(
        h.calling.event_types_for(leg),
        vec!["LEG_ACCEPTED", "LEG_RINGING"]
    );
    // The same event id from ANOTHER provider is a different event (provider-qualified)
    // — and attaches to nothing of provider a.
    let other = h
        .svc
        .ingest_provider_event(
            "dp-b",
            telephony::providers::ProviderEvent {
                provider_event_id: "dup-1".into(),
                provider_call_ref: Some(call_ref.clone()),
                correlation_key: None,
                event: NormalizedEvent::LegAnswered,
                occurred_at: h.now(),
                sequence: Some(2),
                restricted_detail: Default::default(),
                inbound: None,
                dtmf: None,
                provider_cost_minor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(other.as_str(), "QUARANTINED");
    assert_eq!(h.leg_status(leg).await, "RINGING");
    // The database itself refuses a second row for the same provider event.
    assert!(h
        .try_sql(
            "INSERT INTO provider_events (id, provider_id, provider_event_id, disposition, received_at, occurred_at, retain_until, payload_digest) \
             SELECT gen_random_uuid(), provider_id, provider_event_id, disposition, received_at, occurred_at, retain_until, payload_digest \
             FROM provider_events WHERE provider_id = 'dp-a' AND provider_event_id = 'dup-1'"
        )
        .await
        .is_err());
}

#[tokio::test]
async fn out_of_order_events_never_regress_and_late_success_never_resurrects() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, call_ref, correlation) = live_leg(&h).await;
    let t = h.now();
    // CONNECTED arrives before the delayed RINGING / ANSWERED.
    assert_eq!(
        post(&h, &cb("o-3", &call_ref, "connected", 3, t))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(disposition(&h, "o-3").await, "APPLIED");
    let connected = h.leg_status(leg).await;
    assert!(
        matches!(connected.as_str(), "ANSWERED" | "CONNECTED"),
        "{connected}"
    );
    post(&h, &cb("o-1", &call_ref, "ringing", 1, t)).await;
    post(&h, &cb("o-2", &call_ref, "answered", 2, t)).await;
    assert_eq!(disposition(&h, "o-1").await, "STALE");
    assert_eq!(disposition(&h, "o-2").await, "STALE");
    assert_eq!(h.leg_status(leg).await, connected);
    // An unsequenced event that is less advanced than the current state is also stale.
    let unsequenced = Callback {
        event_id: "o-x".into(),
        call_ref: Some(call_ref.clone()),
        correlation_key: None,
        event: "ringing",
        sequence: None,
        at: t,
    };
    post(&h, &unsequenced).await;
    assert_eq!(disposition(&h, "o-x").await, "STALE");
    // The call completes; a late "answered" after that never resurrects the leg.
    post(&h, &cb("o-4", &call_ref, "completed", 4, t)).await;
    assert_eq!(h.leg_status(leg).await, "ENDED");
    post(&h, &cb("o-5", &call_ref, "answered", 5, t)).await;
    assert_eq!(disposition(&h, "o-5").await, "LATE_AFTER_TERMINAL");
    assert_eq!(h.leg_status(leg).await, "ENDED");
    for _ in 0..4 {
        h.run().await;
    }
    let types = h.calling.event_types_for(leg);
    // CONNECTED first implies answered (Calling walks the leg path); the delayed
    // RINGING / ANSWERED and the late success are never delivered after it.
    assert_eq!(
        types,
        vec![
            "LEG_ACCEPTED",
            "LEG_CONNECTED",
            "LEG_ENDED",
            "USAGE_FINALIZED"
        ],
        "{types:?}"
    );
    assert!(h
        .provider_a
        .call(&correlation)
        .unwrap()
        .status
        .is_terminal());
}

#[tokio::test]
async fn a_late_success_on_a_terminal_leg_with_a_live_provider_call_is_cleaned_up() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, call_ref, correlation) = live_leg(&h).await;
    // Calling cancelled the leg; Telephony believes it ended, but the provider call
    // answered late and is still alive.
    let r = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{leg}/terminate"),
            Some(json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(), "idempotencyKey": format!("term-{leg}"),
                         "correlationId": cf_service_auth::CorrelationId::generate().as_str(), "reason": "CANCELLED" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    h.run().await;
    let terminal = h.leg_status(leg).await;
    assert!(
        matches!(terminal.as_str(), "ENDED" | "FAILED"),
        "{terminal}"
    );
    // The provider "missed" the hangup: the call is alive again at the provider.
    let late = h
        .provider_a
        .progress(&correlation, NormalizedEvent::LegAnswered, h.now());
    let d = h.svc.ingest_provider_event("dp-a", late).await.unwrap();
    assert_eq!(d.as_str(), "LATE_AFTER_TERMINAL");
    assert_eq!(h.leg_status(leg).await, terminal);
    // Reconciliation terminates the orphan provider call; nothing reaches Calling.
    let before = h.calling.event_types_for(leg);
    h.advance(10);
    for _ in 0..3 {
        h.run().await;
    }
    assert!(h.provider_a.terminated().contains(&call_ref));
    assert!(h
        .provider_a
        .call(&correlation)
        .unwrap()
        .status
        .is_terminal());
    assert!(!h.calling.event_types_for(leg)[before.len()..]
        .iter()
        .any(|t| t == "LEG_ANSWERED" || t == "LEG_CONNECTED"));
}

#[tokio::test]
async fn an_unknown_provider_object_is_quarantined_never_attached() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, _, _) = live_leg(&h).await;
    h.provider_a.create_unsolicited("dp-unsolicited-1");
    for (id, call_ref, correlation) in [
        ("u-1", Some("dp-unsolicited-1"), None),
        ("u-2", None, Some("cfm-0123456789abcdef0123456789abcdef")),
    ] {
        let r = post(
            &h,
            &Callback {
                event_id: id.into(),
                call_ref: call_ref.map(Into::into),
                correlation_key: correlation.map(Into::into),
                event: "answered",
                sequence: Some(1),
                at: h.now(),
            },
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(disposition(&h, id).await, "QUARANTINED");
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM provider_events WHERE disposition = 'QUARANTINED' AND telephony_leg_id IS NOT NULL")
            .await,
        0
    );
    assert!(
        h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'UNKNOWN_PROVIDER_OBJECT'"
        )
        .await
            >= 1
    );
    assert_eq!(h.leg_status(leg).await, "ORIGINATING");
    for _ in 0..3 {
        h.run().await;
    }
    // Nothing about it reaches Calling.
    assert!(h
        .calling
        .events()
        .iter()
        .all(|e| e["telephonyLegId"] == json!(leg)));
}

#[tokio::test]
async fn raw_payloads_are_never_forwarded_and_delivered_events_follow_the_contract() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, call_ref, _) = live_leg(&h).await;
    let t = h.now();
    for (i, event) in ["ringing", "answered", "connected"].iter().enumerate() {
        post(
            &h,
            &cb(&format!("r-{i}"), &call_ref, event, i as i64 + 1, t),
        )
        .await;
    }
    let body = json!({ "eventId": "r-end", "event": "completed", "occurredAt": t, "callRef": call_ref,
                       "sequence": 9, "costMinor": 250, "currency": "NGN" });
    let raw = serde_json::to_vec(&body).unwrap();
    let sig = h.provider_a.sign(&raw);
    assert_eq!(
        post_raw(&h, "dp-a", raw, Some(sig)).await.status,
        StatusCode::OK
    );
    h.advance(5);
    for _ in 0..8 {
        h.run().await;
    }
    let types = h.calling.event_types_for(leg);
    assert_eq!(
        types,
        vec![
            "LEG_ACCEPTED",
            "LEG_RINGING",
            "LEG_ANSWERED",
            "LEG_CONNECTED",
            "LEG_ENDED",
            "USAGE_FINALIZED"
        ],
        "{types:?}"
    );
    // Every delivered event is the canonical normalized vocabulary — no provider
    // payload, status string, call reference or number.
    assert_contract(&h.calling.events());
    // Stored evidence is bounded, restricted and expires: a digest, the normalized
    // event and a small allow-listed detail — never the raw body.
    assert_eq!(
        h.scalar("SELECT count(*) FROM provider_events WHERE retain_until IS NULL OR payload_digest IS NULL")
            .await,
        0
    );
    let stored = h
        .text("SELECT string_agg(coalesce(restricted_detail::text, ''), ' ') FROM provider_events")
        .await
        .unwrap();
    assert!(
        !stored.contains("eventId") && !stored.contains("occurredAt"),
        "{stored}"
    );
    // Retention: expired evidence is purged by the workers, and a replay of a purged
    // event (now older than the retention window) is ignored, never re-applied.
    let rows = h.scalar("SELECT count(*) FROM provider_events").await;
    assert!(rows >= 4);
    h.advance(3600 + 60);
    h.run().await;
    assert_eq!(h.scalar("SELECT count(*) FROM provider_events").await, 0);
    let replay = cb("r-1", &call_ref, "answered", 2, t);
    assert_eq!(post(&h, &replay).await.status, StatusCode::OK);
    assert_eq!(h.scalar("SELECT count(*) FROM provider_events").await, 0);
    assert_eq!(h.leg_status(leg).await, "ENDED");
    // A provider clock far in the future is ignored as well.
    let future = cb(
        "r-future",
        &call_ref,
        "answered",
        20,
        h.now() + chrono::Duration::hours(2),
    );
    post(&h, &future).await;
    assert_eq!(h.scalar("SELECT count(*) FROM provider_events").await, 0);
}
