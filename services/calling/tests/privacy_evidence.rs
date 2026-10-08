//! Part D §36: privacy-safe call evidence composed by Calling —
//! `GET /v1/privacy-relay/sessions/{id}/evidence`. Ownership is proven by Privacy Relay
//! in the caller's signed scope; the evidence comes from Calling's own records only:
//! canonical timing, normalized outcomes, authoritative connected duration, attempts,
//! decisions — never a number, provider identifier or credential. Keyset pages of at
//! most 100; a foreign session is exactly a missing one.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{calls::CallState, platform::scopes::names};
use common::{
    app::{AppWorld, Device},
    privacy::{FakePrivacyRelay, Mode},
};
use serde_json::Value;
use uuid::Uuid;

async fn world() -> Option<(AppWorld, Arc<FakePrivacyRelay>)> {
    let fake = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let w = AppWorld::with_privacy_relay(fake.clone()).await?;
    fake.allow(w.w.initiator.id().as_uuid());
    fake.allow(w.w.recipient.id().as_uuid());
    Some((w, fake))
}

async fn devices(w: &AppWorld) -> (Device, Device) {
    (
        w.device(&w.w.initiator, "caller-main").await,
        w.device(&w.w.recipient, "callee-main").await,
    )
}

async fn protected_ringing(w: &AppWorld, session: Uuid, key: &str) -> Uuid {
    let (caller, _) = devices(w).await;
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            session,
            key,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

/// A protected call that rings, is answered, connects both parties, stays connected
/// `seconds`, then ends.
async fn protected_completed(w: &AppWorld, session: Uuid, key: &str, seconds: i64) -> Uuid {
    let (caller, callee) = devices(w).await;
    let call = protected_ringing(w, session, key).await;
    let invitation = w.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = w.state(call).await;
    assert_eq!(
        w.accept(call, invitation, &callee, version).await.status,
        StatusCode::OK
    );
    let a = w.session_on(call, caller.endpoint).await.unwrap();
    let b = w.session_on(call, callee.endpoint).await.unwrap();
    w.h().clock.advance(chrono::Duration::seconds(2));
    w.connect(call, a).await;
    w.connect(call, b).await;
    assert_eq!(w.state(call).await.0, CallState::Connected);
    w.h().clock.advance(chrono::Duration::seconds(seconds));
    let (_, version) = w.state(call).await;
    assert_eq!(w.command(call, "end", version).await.status, StatusCode::OK);
    call
}

/// GET with a credential holding exactly `privacy.evidence.read`.
async fn evidence(w: &AppWorld, path: &str) -> common::TestResponse {
    let t = w
        .h()
        .issue(w.w.consumer.sa(w.w.kind), &[names::PRIVACY_EVIDENCE_READ])
        .await;
    w.h()
        .request(Method::GET, path, Some(t.secret.expose_once()), None)
        .await
}

fn evidence_path(session: Uuid) -> String {
    format!("/v1/privacy-relay/sessions/{session}/evidence")
}

#[tokio::test]
async fn evidence_is_composed_from_calling_records_with_authoritative_timing() {
    let Some((w, fake)) = world().await else {
        return;
    };
    let session = Uuid::new_v4();
    fake.own_session(session, w.w.consumer.tenant.id.as_uuid());
    let answered = protected_completed(&w, session, "e-1", 90).await;
    // A missed call: rings, nobody answers, the server expires it (no push needed).
    w.h().clock.advance(chrono::Duration::seconds(1));
    let missed = protected_ringing(&w, session, "e-2").await;
    w.h().clock.advance(chrono::Duration::seconds(46));
    w.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(w.state(missed).await.0, CallState::NoAnswer);
    // An ordinary call and a call of another session are not part of this evidence.
    let other = Uuid::new_v4();
    fake.own_session(other, w.w.consumer.tenant.id.as_uuid());
    protected_ringing(&w, other, "e-3").await;

    let r = evidence(&w, &evidence_path(session)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(r.json["relaySessionId"], session.to_string());
    assert_eq!(r.json["contextId"], fake.context_id.to_string());
    let calls = r.json["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2, "{}", r.text);
    // Newest first.
    assert_eq!(calls[0]["callId"], missed.to_string());
    assert_eq!(calls[1]["callId"], answered.to_string());
    let done = &calls[1];
    assert_eq!(
        (done["state"].as_str(), done["relaySessionId"].as_str()),
        (Some("ENDED"), Some(session.to_string().as_str()))
    );
    for key in [
        "requestedAt",
        "firstAttemptAt",
        "ringingAt",
        "answeredAt",
        "connectedAt",
        "endedAt",
    ] {
        assert!(done[key].is_string(), "{key}: {done}");
    }
    // Authoritative duration: CONNECTED -> end (90 s), never requested -> ended.
    assert_eq!(done["connectedDurationMs"], 90_000);
    let view = w.call_view(answered).await;
    assert_eq!(done["connectedDurationMs"], view["connectedDurationMs"]);
    let parts = done["participants"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert!(parts
        .iter()
        .all(|p| p["connectedDurationMs"].as_i64().unwrap() >= 90_000));
    assert_eq!(done["routeClass"], "APPLICATION");
    assert_eq!(
        done["attemptCount"],
        done["attempts"].as_array().unwrap().len()
    );
    assert_eq!(done["privacyDecisions"][0]["purpose"], "CALL_CREATE");
    // The missed call: NO_ANSWER from the canonical record, no duration.
    let m = &calls[0];
    assert_eq!(
        (m["state"].as_str(), m["terminalReason"].as_str()),
        (Some("NO_ANSWER"), Some("NO_ANSWER"))
    );
    assert!(m["connectedDurationMs"].is_null() && m["connectedAt"].is_null());
    // Privacy safety: nothing number-, provider- or credential-shaped anywhere.
    let lower = r.text.to_lowercase();
    for forbidden in [
        "phone",
        "e164",
        "msisdn",
        "telephone",
        "masked",
        "ciphertext",
        "fingerprint",
        "provider",
        "carrier",
        "sip",
        "cfmk_",
        "secret",
        "token",
        "key_version",
        "binding",
    ] {
        assert!(!lower.contains(forbidden), "{forbidden} in {}", r.text);
    }
    // The response is the canonical contract (no additional members anywhere).
    let contracts =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts");
    let read = |rel: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(contracts.join(rel)).unwrap()).unwrap()
    };
    let mut registry = jsonschema::Registry::new();
    for c in read("manifest.json")["contracts"].as_array().unwrap() {
        if c["kind"] == "json-schema" {
            let doc = read(c["path"].as_str().unwrap());
            let id = doc["$id"].as_str().unwrap().to_string();
            registry = registry
                .add(id, jsonschema::Resource::from_contents(doc))
                .unwrap();
        }
    }
    let registry = registry.prepare().unwrap();
    let validator = jsonschema::options()
        .with_registry(&registry)
        .build(&serde_json::json!({ "$ref": "https://schemas.cfmeet.com/privacy-relay/evidence/v1.schema.json#/$defs/PrivacyRelayEvidence" }))
        .unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&r.json)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    // Ownership was checked with Privacy Relay, in scope.
    assert!(fake
        .proxied
        .lock()
        .unwrap()
        .contains(&format!("/internal/v1/sessions/{session}")));
}

#[tokio::test]
async fn evidence_pages_by_keyset_up_to_one_hundred() {
    let Some((w, fake)) = world().await else {
        return;
    };
    let session = Uuid::new_v4();
    fake.own_session(session, w.w.consumer.tenant.id.as_uuid());
    let mut ids = Vec::new();
    for i in 0..3 {
        w.h().clock.advance(chrono::Duration::seconds(1));
        ids.push(protected_completed(&w, session, &format!("p-{i}"), 5).await);
    }
    ids.reverse();
    let first = evidence(&w, &format!("{}?limit=2", evidence_path(session))).await;
    let page: Vec<Value> = first.json["calls"].as_array().unwrap().clone();
    assert_eq!(page.len(), 2);
    let cursor = first.json["nextCursor"].as_str().unwrap().to_string();
    let second = evidence(
        &w,
        &format!("{}?limit=2&cursor={cursor}", evidence_path(session)),
    )
    .await;
    let rest = second.json["calls"].as_array().unwrap();
    assert_eq!(rest.len(), 1);
    assert!(second.json["nextCursor"].is_null());
    let seen: Vec<String> = page
        .iter()
        .chain(rest.iter())
        .map(|c| c["callId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(seen, ids.iter().map(|i| i.to_string()).collect::<Vec<_>>());
    for bad in [
        "limit=0",
        "limit=101",
        "limit=x",
        "cursor=c1zz",
        "offset=10",
        "limit=1&limit=2",
    ] {
        let r = evidence(&w, &format!("{}?{bad}", evidence_path(session))).await;
        assert_eq!(
            (r.status, r.json["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("VALIDATION_ERROR")),
            "{bad}"
        );
    }
    assert_eq!(
        evidence(&w, &format!("{}?limit=100", evidence_path(session)))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_foreign_session_is_exactly_a_missing_one() {
    let Some((a, fake)) = world().await else {
        return;
    };
    let session = Uuid::new_v4();
    fake.own_session(session, a.w.consumer.tenant.id.as_uuid());
    protected_completed(&a, session, "iso-1", 3).await;
    // Tenant B knows A's session id: 404, identical to a session that does not exist.
    let b = AppWorld::with_privacy_relay(fake.clone()).await.unwrap();
    let foreign = evidence(&b, &evidence_path(session)).await;
    let unknown = evidence(&b, &evidence_path(Uuid::new_v4())).await;
    let malformed = evidence(&b, "/v1/privacy-relay/sessions/not-a-uuid/evidence").await;
    for r in [&foreign, &unknown, &malformed] {
        assert_eq!(
            (r.status, r.json["error"]["code"].as_str()),
            (StatusCode::NOT_FOUND, Some("SESSION_NOT_FOUND")),
            "{}",
            r.text
        );
    }
    let norm = |r: &common::TestResponse| {
        let mut v = r.json.clone();
        v["error"]["requestId"] = Value::Null;
        v
    };
    assert_eq!(norm(&foreign), norm(&unknown));
    // A's own session with B's call records would be empty anyway; B's Calling never
    // sees A's calls.
    assert!(!foreign.text.contains(&session.to_string()));
    // Scope: a credential without privacy.evidence.read is refused, even for its own
    // session; knowing the UUID is not enough.
    let reader = a
        .h()
        .issue(
            a.w.consumer.sa(a.w.kind),
            &[names::CALLS_READ, names::PRIVACY_SESSIONS_READ],
        )
        .await;
    let r = a
        .h()
        .request(
            Method::GET,
            &evidence_path(session),
            Some(reader.secret.expose_once()),
            None,
        )
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("INSUFFICIENT_SCOPE"))
    );
    // Privacy Relay unavailable: fail closed (no evidence without proven ownership).
    fake.set_mode(Mode::Unavailable);
    let r = evidence(&a, &evidence_path(session)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(r.json.get("calls").is_none());
}
